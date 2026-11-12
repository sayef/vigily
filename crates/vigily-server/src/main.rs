//! `vigily-engine`: serves one repository's API and web app. `vigily up` runs it in the
//! foreground; clients start it in the background (`--detach`) when no engine serves their
//! repository yet.

// The server's start-up future (storage, records, scheduler, monitor) nests deeply.
#![recursion_limit = "256"]

use std::net::IpAddr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use vigily_server::engine::{self, Mode, Options};

#[derive(Parser)]
#[command(
    name = "vigily-engine",
    version,
    about = "Serve a vigily repository's API and web app"
)]
struct Args {
    /// The repository: a directory, or an object-store URI such as s3://bucket/prefix.
    #[arg(long, short, env = "VIGILY_REPO", default_value = ".vigily")]
    repo: String,
    /// Address to listen on. Anything but a loopback address serves every host name.
    #[arg(long, default_value = "127.0.0.1")]
    host: IpAddr,
    #[arg(long, short, env = "VIGILY_PORT", default_value_t = vigily_api::local::DEFAULT_PORT)]
    port: u16,
    /// Do not optimize storage in the background (on by default).
    #[arg(long)]
    no_optimize: bool,
    /// How often to optimize storage in the background: `15m`, `1h`, ...
    #[arg(long, default_value = "15m", value_parser = parse_every)]
    optimize_every: Duration,
    /// Also optimize storage before serving (slower start-up; see docs/architecture.md).
    #[arg(long)]
    optimize_at_start: bool,
    /// The server configuration: schedules, alert webhook, drift thresholds. Defaults to
    /// ./vigily.toml when it exists.
    #[arg(long, env = "VIGILY_CONFIG")]
    config: Option<PathBuf>,
    /// `local`: one user on this machine, with the repository's token. `server`: a shared
    /// engine that needs VIGILY_ADMIN_TOKEN (or VIGILY_ADMIN_TOKEN_FILE) and accepts scoped
    /// tokens. Defaults to local on a loopback address and server on any other.
    #[arg(long, value_enum)]
    mode: Option<ModeArg>,
    /// Where engines note their address and keep their token and log. Defaults to
    /// $VIGILY_STATE_DIR, else ~/Library/Caches/vigily on macOS and ~/.cache/vigily elsewhere.
    #[arg(long)]
    state_dir: Option<PathBuf>,
    /// Stop after this long without requests, active runs or schedules (`30m`, `2h`; `0s` never).
    /// Engines started with --detach default to 30m; others run until stopped.
    #[arg(long, value_parser = vigily_server::config::parse_duration)]
    idle_timeout: Option<Duration>,
    /// Start in the background, print the address once it answers, and return. Output goes to
    /// engines/<key>.log in the state directory.
    #[arg(long)]
    detach: bool,
    /// Check a configuration file and exit: 0 when it is valid, 1 with the problem otherwise.
    #[arg(long, value_name = "FILE")]
    check_config: Option<PathBuf>,
    /// Where the local token is kept. Defaults to engines/<key>.token in the state directory.
    #[arg(long, env = "VIGILY_TOKEN_FILE")]
    token_file: Option<PathBuf>,
    /// Check no tokens at all. Anyone who can reach the port can read runs, change them and
    /// run schedules: only behind your own access control.
    #[arg(long)]
    insecure_no_auth: bool,
    /// Set by --detach on the process it starts.
    #[arg(long, hide = true)]
    auto_started: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum ModeArg {
    Local,
    Server,
}

/// The shortest background optimize interval: an optimize takes seconds on a large repository, so
/// anything shorter would run them back to back.
const MIN_OPTIMIZE_EVERY: Duration = Duration::from_secs(60);

fn parse_every(s: &str) -> Result<Duration, String> {
    let d = vigily_server::config::parse_duration(s)?;
    if d < MIN_OPTIMIZE_EVERY {
        return Err(format!(
            "{s} is too often; use 1m or more, or --no-optimize to turn background optimizing off"
        ));
    }
    Ok(d)
}

impl Args {
    fn options(&self) -> Options {
        Options {
            repo: self.repo.clone(),
            host: self.host,
            port: self.port,
            optimize: !self.no_optimize,
            optimize_every: self.optimize_every,
            optimize_at_start: self.optimize_at_start,
            config: self.config.clone(),
            mode: self.mode.map(|m| match m {
                ModeArg::Local => Mode::Local,
                ModeArg::Server => Mode::Server,
            }),
            state_dir: self.state_dir.clone(),
            idle_timeout: self.idle_timeout,
            auto_started: self.auto_started,
            token_file: self.token_file.clone(),
            insecure_no_auth: self.insecure_no_auth,
        }
    }
}

fn main() -> ExitCode {
    let args = Args::parse();
    if let Some(file) = &args.check_config {
        return match vigily_server::config::Config::load(file) {
            Ok(c) => {
                let n = c.schedule.len();
                println!(
                    "{}: valid, {n} {}",
                    file.display(),
                    if n == 1 { "schedule" } else { "schedules" }
                );
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        };
    }
    let result = if args.detach {
        detach(&args)
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_env("VIGILY_LOG")
                    .unwrap_or_else(|_| "warn,vigily=info".into()),
            )
            .with_writer(std::io::stderr)
            .init();
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .context("could not start the runtime")
            .and_then(|rt| rt.block_on(engine::run(args.options())))
    };
    result.unwrap_or_else(|e| {
        eprintln!("error: {e:#}");
        ExitCode::FAILURE
    })
}

/// Starts this engine again in the background, waits until it answers (at most 30 s) and prints
/// its address. Exits with the engine's code when it stops before that.
fn detach(args: &Args) -> Result<ExitCode> {
    let (_, paths) = args.options().paths()?;
    engine::private_dir(&paths.dir)
        .with_context(|| format!("could not create {}", paths.dir.display()))?;
    let log = engine::private_file(&paths.log)
        .with_context(|| format!("could not create {}", paths.log.display()))?;
    let exe = std::env::current_exe().context("could not find this program")?;
    let mut cmd = std::process::Command::new(exe);
    cmd.args(std::env::args_os().skip(1).filter(|a| a != "--detach"))
        .arg("--auto-started")
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    // Its own process group: Ctrl-C in this terminal does not reach it.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    let mut child = cmd.spawn().context("could not start the engine")?;
    for _ in 0..300 {
        if let Some(f) = paths.read().filter(|f| f.pid == child.id()) {
            println!("{}", f.url);
            eprintln!(
                "vigily-engine serving {} at {} (pid {}, log {})",
                args.repo,
                f.url,
                f.pid,
                paths.log.display()
            );
            return Ok(ExitCode::SUCCESS);
        }
        if let Some(status) = child.try_wait()? {
            let tail = std::fs::read_to_string(&paths.log).unwrap_or_default();
            let tail: Vec<&str> = tail.lines().rev().take(5).collect();
            for line in tail.iter().rev() {
                eprintln!("{line}");
            }
            let code = status.code().unwrap_or(1).clamp(0, 255) as u8;
            return Ok(ExitCode::from(code.max(1)));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    anyhow::bail!(
        "the engine did not answer within 30 seconds; see {}",
        paths.log.display()
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn optimize_every_has_a_minimum() {
        assert!(super::parse_every("0s").is_err());
        assert!(super::parse_every("30s").is_err());
        assert_eq!(super::parse_every("1m").unwrap().as_secs(), 60);
        assert_eq!(super::parse_every("15m").unwrap().as_secs(), 900);
    }
}
