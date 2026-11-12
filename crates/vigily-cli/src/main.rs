//! `vigily`: serve the web app and API over a repository, list runs, compact storage.

// The server's start-up future (storage, records, scheduler, monitor) nests deeply.
#![recursion_limit = "256"]

use std::net::{IpAddr, SocketAddr};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use vigily_core::{now_micros, Catalog, Query};
use vigily_server::{serve, AppState, AuthSetup, ServeOptions};

const DEFAULT_PORT: u16 = 7600;

#[derive(Parser)]
#[command(
    name = "vigily",
    version,
    about = "An experiment tracker that keeps watch over runs, evaluations and drift"
)]
struct Cli {
    /// The repository: a directory, or an object-store URI such as s3://bucket/prefix.
    #[arg(
        long,
        short,
        global = true,
        env = "VIGILY_REPO",
        default_value = ".vigily"
    )]
    repo: String,
    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum Mode {
    Local,
    Server,
}

/// The local token's default file: one per repository under the state directory.
fn default_token_file(repo: &str) -> Result<std::path::PathBuf> {
    let dir = match std::env::var_os("VIGILY_STATE_DIR").filter(|d| !d.is_empty()) {
        Some(d) => std::path::PathBuf::from(d),
        None => {
            let home = std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .context("no home directory to keep the local token in; pass --token-file")?;
            std::path::PathBuf::from(home).join(".cache").join("vigily")
        }
    };
    // The same repository under another spelling (./runs, runs/) gets the same token.
    let key = if repo.contains("://") {
        repo.to_string()
    } else {
        std::fs::canonicalize(repo)
            .unwrap_or_else(|_| std::path::PathBuf::from(repo))
            .to_string_lossy()
            .to_string()
    };
    let hash = vigily_core::tokens::hash(&key);
    Ok(dir.join("tokens").join(format!("{}.token", &hash[..16])))
}

#[derive(Subcommand)]
enum Command {
    /// Serve the web app and the API.
    Up {
        /// Address to listen on. Anything but a loopback address serves every host name.
        #[arg(long, default_value = "127.0.0.1")]
        host: IpAddr,
        #[arg(long, short, env = "VIGILY_PORT", default_value_t = DEFAULT_PORT)]
        port: u16,
        /// Do not optimize storage in the background (on by default).
        #[arg(long)]
        no_optimize: bool,
        /// How often to optimize storage in the background: `15m`, `1h`, ...
        #[arg(long, default_value = "15m", value_parser = parse_every)]
        optimize_every: std::time::Duration,
        /// Also optimize storage before serving (slower start-up; see docs/architecture.md).
        #[arg(long)]
        optimize_at_start: bool,
        /// The server configuration: schedules, alert webhook, drift thresholds. Defaults to
        /// ./vigily.toml when it exists.
        #[arg(long, env = "VIGILY_CONFIG")]
        config: Option<std::path::PathBuf>,
        /// `local`: one user on this machine, with the repository's token. `server`: a shared
        /// engine that needs VIGILY_ADMIN_TOKEN (or VIGILY_ADMIN_TOKEN_FILE) and accepts scoped
        /// tokens. Defaults to local on a loopback address and server on any other.
        #[arg(long, value_enum)]
        mode: Option<Mode>,
        /// Where the local token is kept. Defaults to a file per repository under
        /// $VIGILY_STATE_DIR (or ~/.cache/vigily)/tokens.
        #[arg(long, env = "VIGILY_TOKEN_FILE")]
        token_file: Option<std::path::PathBuf>,
        /// Check no tokens at all. Anyone who can reach the port can read runs, change them and
        /// run schedules: only behind your own access control.
        #[arg(long)]
        insecure_no_auth: bool,
    },
    /// Delete traces by id; with --erase, also erase them from storage (as `vigily runs purge`).
    Traces {
        #[command(subcommand)]
        action: TracesAction,
    },
    /// List runs, newest first; `vigily runs purge` erases deleted runs from storage.
    #[command(args_conflicts_with_subcommands = true)]
    Runs {
        #[command(subcommand)]
        action: Option<RunsAction>,
        /// A run query, as in the web app: `status:failed lr<0.01 loss[val]<0.3`.
        #[arg(default_value = "")]
        query: String,
        /// Sort by a column: run:name, param:<key> or metric:<last|min|max>:<metric>.
        #[arg(long)]
        sort: Option<String>,
        /// Sort descending.
        #[arg(long)]
        desc: bool,
        /// Show at most this many runs.
        #[arg(long, short = 'n', default_value_t = 50)]
        limit: usize,
    },
    /// Compact storage and build the metrics index. Run it when training is done.
    Optimize,
    /// List or run the schedules of a configuration file.
    Schedules {
        /// The configuration file (default ./vigily.toml).
        #[arg(long, env = "VIGILY_CONFIG", default_value = "vigily.toml")]
        config: std::path::PathBuf,
        #[command(subcommand)]
        action: Option<ScheduleAction>,
    },
}

#[derive(Subcommand)]
enum TracesAction {
    /// Delete traces (their summaries and spans). Traces linked to a run are deleted with the run;
    /// this is for traces without one.
    Delete {
        /// Trace ids.
        #[arg(required = true)]
        ids: Vec<String>,
        /// Then erase them from storage: compact, rewrite the files that held them and remove
        /// older versions (this also erases every run deleted so far).
        #[arg(long)]
        erase: bool,
        /// With --erase, also delete storage tags that keep deleted data.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum RunsAction {
    /// Erase deleted runs from storage for good: rewrite the files that still hold their data and
    /// remove older versions. Deleting in the web app only hides runs until this runs. Run it
    /// when a short burst of reader refreshes is acceptable; training can keep writing.
    Purge {
        /// Also delete storage tags that keep deleted data (they are named when the purge stops).
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum ScheduleAction {
    /// Run a schedule's command now, as the operator, and record it like a scheduled run.
    Run { name: String },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("VIGILY_LOG")
                .unwrap_or_else(|_| "warn,vigily=info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let open = || async {
        let create = matches!(cli.command, Command::Up { .. } | Command::Schedules { .. });
        if !create && !cli.repo.contains("://") && !std::path::Path::new(&cli.repo).exists() {
            anyhow::bail!(
                "no repository at {}. Log a run with the Python SDK first, or pass --repo",
                cli.repo
            );
        }
        vigily_core::open_store(&cli.repo)
            .await
            .with_context(|| format!("could not open the repository at {}", cli.repo))
    };
    match &cli.command {
        Command::Up {
            host,
            port,
            no_optimize,
            optimize_every,
            optimize_at_start,
            config,
            mode,
            token_file,
            insecure_no_auth,
        } => {
            let server = match mode {
                Some(m) => *m == Mode::Server,
                None => !host.is_loopback(),
            };
            let auth = if *insecure_no_auth {
                AuthSetup::Off { server }
            } else if server {
                match vigily_server::auth::admin_token_from_env().map_err(|e| anyhow::anyhow!(e))? {
                    Some(admin_token) => AuthSetup::Server { admin_token },
                    None => anyhow::bail!(
                        "server mode needs an admin token: set VIGILY_ADMIN_TOKEN (or VIGILY_ADMIN_TOKEN_FILE) \
                         to a long random value, such as the output of `openssl rand -hex 32`. \
                         To serve without any sign-in behind your own access control, pass --insecure-no-auth"
                    ),
                }
            } else {
                AuthSetup::Local {
                    token_file: match token_file {
                        Some(f) => f.clone(),
                        None => default_token_file(&cli.repo)?,
                    },
                }
            };
            let config_path = config.clone().or_else(|| {
                let p = std::path::PathBuf::from("vigily.toml");
                p.exists().then_some(p)
            });
            let config = match &config_path {
                Some(p) => {
                    vigily_server::config::Config::load(p).map_err(|e| anyhow::anyhow!(e))?
                }
                None => vigily_server::config::Config::default(),
            };
            let store = open().await?;
            let records = vigily_core::open_records(&cli.repo)
                .await
                .with_context(|| format!("could not open the records of {}", cli.repo))?;
            if let Some(p) = &config_path {
                eprintln!(
                    "configuration {}: {} {}",
                    p.display(),
                    config.schedule.len(),
                    if config.schedule.len() == 1 {
                        "schedule"
                    } else {
                        "schedules"
                    }
                );
            }
            let state = AppState::with(store, records, config);
            let opts = ServeOptions {
                addr: SocketAddr::new(*host, *port),
                local_only: host.is_loopback(),
                optimize: !no_optimize,
                optimize_every: *optimize_every,
                optimize_at_start: *optimize_at_start,
                auth: auth.clone(),
            };
            let repo = cli.repo.clone();
            eprintln!("vigily {} reading {repo}", env!("CARGO_PKG_VERSION"));
            if let AuthSetup::Off { .. } = auth {
                eprintln!(
                    "warning: serving without sign-in (--insecure-no-auth): anyone who can reach this port can \
                     read and change runs and start schedules. Put vigily behind your own access control."
                );
            }
            if !vigily_ui::is_built() {
                eprintln!("warning: built without the web app; only the API is served");
            }
            serve(state, opts, |ready| match (&ready.sign_in_url, &auth) {
                (Some(link), _) => eprintln!("Open {link} (Ctrl-C to stop)"),
                (None, AuthSetup::Server { .. }) => eprintln!(
                    "Serving {} in server mode: sign in with the admin token or an access token (Ctrl-C to stop)",
                    ready.url
                ),
                (None, _) => eprintln!("Open {} (Ctrl-C to stop)", ready.url),
            })
            .await
            .with_context(|| format!("could not listen on {host}:{port}"))?;
        }
        Command::Traces {
            action: TracesAction::Delete { ids, erase, force },
        } => {
            let records = vigily_core::open_records(&cli.repo).await?;
            let mut deleted = 0;
            for id in ids {
                if vigily_core::traces::delete(&*records, id).await? {
                    deleted += 1;
                } else {
                    eprintln!("No trace with id {id}.");
                }
            }
            println!(
                "Deleted {deleted} {}.",
                if deleted == 1 { "trace" } else { "traces" }
            );
            if *erase {
                let store = open().await?;
                let r = store.purge(*force).await?;
                println!(
                    "Erased: {} files rewritten, {} old versions and {} removed. Not reached: backups, \
                     object-store versioning or replication, and disk blocks (see the storage guide).",
                    r.fragments_rewritten,
                    r.versions_removed,
                    bytes(r.bytes_removed)
                );
            } else {
                println!("Deleting hides them; `vigily runs purge` (or --erase) erases them from storage.");
            }
        }
        Command::Runs {
            action: Some(RunsAction::Purge { force }),
            ..
        } => {
            let store = open().await?;
            let legacy = format!("{}/records.lance", cli.repo.trim_end_matches('/'));
            match store.purge(*force).await {
                Ok(r) => {
                    println!(
                        "Purged {} deleted {}: {} files rewritten, {} old versions and {} removed.",
                        r.runs,
                        if r.runs == 1 { "run" } else { "runs" },
                        r.fragments_rewritten,
                        r.versions_removed,
                        bytes(r.bytes_removed)
                    );
                    if !r.tags_deleted.is_empty() {
                        println!(
                            "Tags deleted because they kept erased data: {}",
                            r.tags_deleted.join(", ")
                        );
                    }
                }
                Err(e) => {
                    let msg = e.to_string();
                    if msg.contains("tag") && !*force {
                        anyhow::bail!(vigily_server::admin_api::tag_blocks_purge_message(&msg));
                    }
                    return Err(e.into());
                }
            }
            let records = vigily_core::open_records(&cli.repo).await?;
            if records.legacy_present().await? {
                eprintln!(
                    "Note: {legacy} is the records table of an earlier version, already migrated. Purge does not \
                     reach it: it still holds every record as of the upgrade. Delete it by hand to erase them."
                );
            }
            println!(
                "Not reached by a purge: backups, object-store versioning or replication, other machines' \
                 write-ahead logs of crashed writers, and disk blocks (see the storage guide)."
            );
        }
        Command::Runs {
            action: None,
            query,
            sort,
            desc,
            limit,
        } => {
            let q = Query::parse(query).map_err(|e| anyhow::anyhow!("{e}"))?;
            let catalog = Catalog::new(open().await?);
            let snap = catalog.refresh().await?;
            let rows = snap.select(&q, sort.as_deref(), *desc, false);
            let now = now_micros();
            println!(
                "{:<10} {:<28} {:<16} {:<9} {:>10}",
                "ID", "NAME", "EXPERIMENT", "STATUS", "DURATION"
            );
            for e in rows.iter().take(*limit) {
                let r = &e.run;
                println!(
                    "{:<10} {:<28} {:<16} {:<9} {:>10}",
                    r.short_id(),
                    clip(&r.display_name(), 28),
                    clip(r.experiment.as_deref().unwrap_or("-"), 16),
                    r.status.as_str(),
                    duration(r.duration_secs(now)),
                );
            }
            if rows.len() > *limit {
                eprintln!("{} more; raise --limit to see them", rows.len() - limit);
            }
        }
        Command::Schedules { config, action } => {
            let cfg =
                vigily_server::config::Config::load(config).map_err(|e| anyhow::anyhow!(e))?;
            match action {
                None => {
                    for s in &cfg.schedule {
                        println!(
                            "{:<24} {:<14} {:<16} {}{}",
                            s.name,
                            s.cron,
                            s.timezone,
                            s.command.join(" "),
                            if s.enabled { "" } else { "  (disabled)" }
                        );
                    }
                }
                Some(ScheduleAction::Run { name }) => {
                    let s = cfg
                        .schedule
                        .iter()
                        .find(|s| &s.name == name)
                        .cloned()
                        .ok_or_else(|| {
                            anyhow::anyhow!("no schedule named {name} in {}", config.display())
                        })?;
                    let store = vigily_core::open_store(&cli.repo).await?;
                    let records = vigily_core::open_records(&cli.repo).await?;
                    let state = AppState::with(store, records, cfg);
                    let runs = vigily_server::scheduler::execute(&state, &s, now_micros()).await;
                    state.records.flush().await?;
                    for r in &runs {
                        eprintln!(
                            "attempt {}: {:?}, exit {:?}",
                            r.attempt, r.status, r.exit_code
                        );
                    }
                    if runs.last().map(|r| r.status)
                        != Some(vigily_core::evals::ScheduleRunStatus::Succeeded)
                    {
                        std::process::exit(1);
                    }
                }
            }
        }
        Command::Optimize => {
            let store = open().await?;
            store.optimize().await?;
            eprintln!("Optimized {}", cli.repo);
        }
    }
    Ok(())
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(n - 1).collect();
        t.push('…');
        t
    }
}

fn duration(secs: f64) -> String {
    let s = secs as u64;
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m {:02}s", s / 60, s % 60),
        _ => format!("{}h {:02}m", s / 3600, (s % 3600) / 60),
    }
}

/// A byte count for people: 1.2 MB, 3.4 GB.
fn bytes(n: u64) -> String {
    let n = n as f64;
    match n {
        n if n >= 1e9 => format!("{:.1} GB", n / 1e9),
        n if n >= 1e6 => format!("{:.1} MB", n / 1e6),
        n if n >= 1e3 => format!("{:.1} kB", n / 1e3),
        n => format!("{n} B"),
    }
}

/// The shortest background optimize interval: an optimize takes seconds on a large repository, so
/// anything shorter would run them back to back.
const MIN_OPTIMIZE_EVERY: std::time::Duration = std::time::Duration::from_secs(60);

fn parse_every(s: &str) -> Result<std::time::Duration, String> {
    let d = vigily_server::config::parse_duration(s)?;
    if d < MIN_OPTIMIZE_EVERY {
        return Err(format!(
            "{s} is too often; use 1m or more, or --no-optimize to turn background optimizing off"
        ));
    }
    Ok(d)
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
