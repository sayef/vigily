//! The engine process: one per repository on a machine, found by clients through its state file.
//!
//! [`run`] takes the repository's lock (`engines/<key>.lock`, see [`vigily_api::local`]), keeps
//! the local token in `engines/<key>.token`, serves, writes `engines/<key>.json` once it listens
//! and removes it on exit. An engine started in the background stops after a while without use.

use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use vigily_api::local::{self, EngineFile, EnginePaths};
use vigily_core::now_micros;

use crate::{serve, AppState, AuthSetup, ServeOptions};

/// The exit code of an engine that finds its repository already served.
pub const EXIT_ALREADY_SERVED: u8 = 3;

/// How long an engine started in the background waits without use before it stops.
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// A run that showed a sign of life this recently keeps an idle engine running.
pub const RUN_ACTIVITY: Duration = Duration::from_secs(10 * 60);

/// Who the engine serves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// One user on this machine, with the repository's token.
    Local,
    /// A shared engine with an admin token and scoped tokens.
    Server,
}

/// What [`run`] needs; the `vigily-engine` flags.
#[derive(Clone, Debug)]
pub struct Options {
    /// A directory or an object-store URI.
    pub repo: String,
    pub host: IpAddr,
    pub port: u16,
    pub optimize: bool,
    pub optimize_every: Duration,
    pub optimize_at_start: bool,
    /// The configuration file; `./vigily.toml` when it exists and none is given.
    pub config: Option<PathBuf>,
    /// Local on a loopback address and server on any other, unless given.
    pub mode: Option<Mode>,
    /// The state directory; [`local::state_dir`] when not given.
    pub state_dir: Option<PathBuf>,
    /// Stop after this long without use. [`DEFAULT_IDLE_TIMEOUT`] for an engine started in the
    /// background; a foreground engine never stops on its own unless given one.
    pub idle_timeout: Option<Duration>,
    /// Started in the background: idles out, and takes any free port when its own is taken.
    pub auto_started: bool,
    /// Where the local token is kept; `engines/<key>.token` in the state directory by default.
    pub token_file: Option<PathBuf>,
    pub insecure_no_auth: bool,
}

impl Options {
    /// The defaults of `vigily-engine --repo <repo>`.
    pub fn new(repo: &str) -> Self {
        Options {
            repo: repo.to_string(),
            host: IpAddr::from([127, 0, 0, 1]),
            port: local::DEFAULT_PORT,
            optimize: true,
            optimize_every: crate::OPTIMIZE_EVERY,
            optimize_at_start: false,
            config: None,
            mode: None,
            state_dir: None,
            idle_timeout: None,
            auto_started: false,
            token_file: None,
            insecure_no_auth: false,
        }
    }

    /// The state directory and this repository's files in it.
    pub fn paths(&self) -> Result<(PathBuf, EnginePaths)> {
        let dir = match &self.state_dir {
            Some(d) => d.clone(),
            None => local::state_dir().context(
                "no home directory for vigily's state; set VIGILY_STATE_DIR or pass --state-dir",
            )?,
        };
        let paths = EnginePaths::new(&dir, &local::canonical_location(&self.repo));
        Ok((dir, paths))
    }
}

/// Creates `dir` and its parents, private to this user (0700).
pub fn private_dir(dir: &Path) -> std::io::Result<()> {
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
    b.create(dir)
}

/// Opens `path` for writing, private to this user (0600), truncated.
pub fn private_file(path: &Path) -> std::io::Result<std::fs::File> {
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    o.open(path)
}

/// Takes the repository's lock, retrying for a moment (a client checking the lock holds it
/// shared for an instant). `None` when another engine holds it.
fn lock(paths: &EnginePaths) -> Result<Option<std::fs::File>> {
    let mut o = std::fs::OpenOptions::new();
    o.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    let file = o
        .open(&paths.lock)
        .with_context(|| format!("could not open {}", paths.lock.display()))?;
    for _ in 0..20 {
        match file.try_lock() {
            Ok(()) => return Ok(Some(file)),
            Err(std::fs::TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(50)),
            Err(std::fs::TryLockError::Error(e)) => {
                return Err(e).with_context(|| format!("could not lock {}", paths.lock.display()))
            }
        }
    }
    Ok(None)
}

/// Writes `file` atomically (a private temporary file renamed into place).
pub fn write_engine_file(path: &Path, file: &EngineFile) -> std::io::Result<()> {
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    let mut f = private_file(&tmp)?;
    let body = serde_json::to_vec_pretty(file).map_err(std::io::Error::other)?;
    std::io::Write::write_all(&mut f, &body)?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)
}

/// Removes the engine file on exit, if it is still this process's.
struct EngineFileGuard(PathBuf);

impl Drop for EngineFileGuard {
    fn drop(&mut self) {
        let ours = std::fs::read_to_string(&self.0)
            .ok()
            .and_then(|s| serde_json::from_str::<EngineFile>(&s).ok())
            .is_some_and(|f| f.pid == std::process::id());
        if ours {
            let _ = std::fs::remove_file(&self.0);
        }
    }
}

/// The local token file of earlier releases (`tokens/<key>.token` under `$VIGILY_STATE_DIR` or
/// `~/.cache/vigily`), whose key was the SHA-256 of the canonical path or the URI as given.
fn legacy_token_files(state_dir: &Path, repo: &str) -> Vec<PathBuf> {
    let key = if repo.contains("://") {
        repo.to_string()
    } else {
        std::fs::canonicalize(repo)
            .unwrap_or_else(|_| PathBuf::from(repo))
            .to_string_lossy()
            .to_string()
    };
    let name = format!("{}.token", &local::sha256_hex(&key)[..16]);
    let mut dirs = vec![state_dir.to_path_buf()];
    let legacy = match std::env::var_os("VIGILY_STATE_DIR").filter(|d| !d.is_empty()) {
        Some(d) => Some(PathBuf::from(d)),
        None => std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .map(|h| PathBuf::from(h).join(".cache").join("vigily")),
    };
    if let Some(d) = legacy.filter(|d| d != state_dir) {
        dirs.push(d);
    }
    dirs.into_iter()
        .map(|d| d.join("tokens").join(&name))
        .collect()
}

/// Moves a local token from where earlier releases kept it, so links and scripts keep working.
fn migrate_token(state_dir: &Path, repo: &str, to: &Path) {
    if to.exists() {
        return;
    }
    for from in legacy_token_files(state_dir, repo) {
        if !from.is_file() {
            continue;
        }
        let moved = std::fs::rename(&from, to).or_else(|_| {
            std::fs::copy(&from, to)?;
            std::fs::remove_file(&from)
        });
        match moved {
            Ok(()) => {
                tracing::info!(from = %from.display(), to = %to.display(), "moved the local token");
                return;
            }
            Err(e) => {
                tracing::warn!(from = %from.display(), error = %e, "could not move the local token")
            }
        }
    }
}

/// Whether the engine has had nothing to do for `timeout`: no request, no run with a sign of life
/// in [`RUN_ACTIVITY`], and no enabled schedule.
pub fn is_idle(state: &AppState, timeout: Duration) -> bool {
    let now = now_micros();
    let quiet = now - state.last_request.load(Ordering::Relaxed) >= timeout.as_micros() as i64;
    let scheduled = state.config.schedule.iter().any(|s| s.enabled);
    let active = state.catalog.snapshot().runs.iter().any(|e| {
        e.run.status == vigily_core::RunStatus::Running
            && now - e.last_seen() < RUN_ACTIVITY.as_micros() as i64
    });
    quiet && !scheduled && !active
}

/// Serves `opts.repo` until stopped, idle (when it has an idle timeout) or Ctrl-C/SIGTERM.
/// [`EXIT_ALREADY_SERVED`] when another engine serves the repository.
pub async fn run(opts: Options) -> Result<ExitCode> {
    let (state_dir, paths) = opts.paths()?;
    private_dir(&paths.dir).with_context(|| format!("could not create {}", paths.dir.display()))?;
    let Some(_lock) = lock(&paths)? else {
        // The other engine may still be starting: give it a moment to write its address.
        let mut url = None;
        for _ in 0..50 {
            if let Some(f) = paths.read() {
                url = Some(f.url);
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        match url {
            Some(url) => eprintln!("error: {} is already served at {url}", opts.repo),
            None => eprintln!(
                "error: {} is already served by another engine (still starting; see {})",
                opts.repo,
                paths.log.display()
            ),
        }
        return Ok(ExitCode::from(EXIT_ALREADY_SERVED));
    };
    let server = match opts.mode {
        Some(m) => m == Mode::Server,
        None => !opts.host.is_loopback(),
    };
    let auth = if opts.insecure_no_auth {
        AuthSetup::Off { server }
    } else if server {
        match crate::auth::admin_token_from_env().map_err(|e| anyhow::anyhow!(e))? {
            Some(admin_token) => AuthSetup::Server { admin_token },
            None => anyhow::bail!(
                "server mode needs an admin token: set VIGILY_ADMIN_TOKEN (or VIGILY_ADMIN_TOKEN_FILE) \
                 to a long random value, such as the output of `openssl rand -hex 32`. \
                 To serve without any sign-in behind your own access control, pass --insecure-no-auth"
            ),
        }
    } else {
        let token_file = match &opts.token_file {
            Some(f) => f.clone(),
            None => {
                migrate_token(&state_dir, &opts.repo, &paths.token);
                paths.token.clone()
            }
        };
        AuthSetup::Local { token_file }
    };
    let config_path = opts.config.clone().or_else(|| {
        let p = PathBuf::from("vigily.toml");
        p.exists().then_some(p)
    });
    let config = match &config_path {
        Some(p) => crate::config::Config::load(p).map_err(|e| anyhow::anyhow!(e))?,
        None => crate::config::Config::default(),
    };
    let store = vigily_core::open_store(&opts.repo)
        .await
        .with_context(|| format!("could not open the repository at {}", opts.repo))?;
    let records = vigily_core::open_records(&opts.repo)
        .await
        .with_context(|| format!("could not open the records of {}", opts.repo))?;
    if let Some(p) = &config_path {
        let n = config.schedule.len();
        eprintln!(
            "configuration {}: {n} {}",
            p.display(),
            if n == 1 { "schedule" } else { "schedules" }
        );
    }
    let state = AppState::with(store, records, config);
    let idle_timeout = opts
        .idle_timeout
        .or(opts.auto_started.then_some(DEFAULT_IDLE_TIMEOUT))
        .filter(|t| !t.is_zero());
    let stop = Arc::new(tokio::sync::Notify::new());
    if let Some(timeout) = idle_timeout {
        let (bg, stop) = (state.clone(), stop.clone());
        let every = (timeout / 4).clamp(Duration::from_millis(100), Duration::from_secs(30));
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(every).await;
                if is_idle(&bg, timeout) {
                    tracing::info!(?timeout, "no use; stopping");
                    stop.notify_one();
                    break;
                }
            }
        });
    }
    let serve_opts = ServeOptions {
        addr: SocketAddr::new(opts.host, opts.port),
        local_only: opts.host.is_loopback(),
        optimize: opts.optimize,
        optimize_every: opts.optimize_every,
        optimize_at_start: opts.optimize_at_start,
        auth: auth.clone(),
        port_fallback: opts.auto_started,
        stop: Some(stop),
    };
    eprintln!("vigily {} reading {}", env!("CARGO_PKG_VERSION"), opts.repo);
    if let AuthSetup::Off { .. } = auth {
        eprintln!(
            "warning: serving without sign-in (--insecure-no-auth): anyone who can reach this port can \
             read and change runs and start schedules. Put vigily behind your own access control."
        );
    }
    if !vigily_ui::is_built() {
        eprintln!("warning: built without the web app; only the API is served");
    }
    let mut guard = None;
    let mut written = Ok(());
    let mode = match auth {
        AuthSetup::Off { server: true } | AuthSetup::Server { .. } => "server",
        AuthSetup::Off { server: false } | AuthSetup::Local { .. } => "local",
    };
    let canonical = local::canonical_location(&opts.repo);
    let (host, port) = (opts.host, opts.port);
    let st = state.clone();
    serve(state, serve_opts, |ready| {
        let file = EngineFile {
            url: ready.url.clone(),
            pid: std::process::id(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            api_version: vigily_api::API_VERSION.to_string(),
            repo: canonical,
            token: ready.token.clone(),
            mode: mode.to_string(),
            started_at: now_micros(),
            auto_started: opts.auto_started,
        };
        written = write_engine_file(&paths.json, &file);
        if written.is_ok() {
            guard = Some(EngineFileGuard(paths.json.clone()));
            *st.engine_file.write() = Some(paths.json.clone());
        }
        let hint = if idle_timeout.is_some() {
            "stops when idle"
        } else {
            "Ctrl-C to stop"
        };
        match (&ready.sign_in_url, &auth) {
            (Some(link), _) => eprintln!("Open {link} ({hint})"),
            (None, AuthSetup::Server { .. }) => eprintln!(
                "Serving {} in server mode: sign in with the admin token or an access token ({hint})",
                ready.url
            ),
            (None, _) => eprintln!("Open {} ({hint})", ready.url),
        }
    })
    .await
    .with_context(|| format!("could not listen on {host}:{port}"))?;
    if let Err(e) = written {
        tracing::warn!(file = %paths.json.display(), error = %e, "could not write the engine file");
    }
    drop(guard);
    Ok(ExitCode::SUCCESS)
}
