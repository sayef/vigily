//! The `vigily` command: a client of the engine that serves the repository.
//!
//! Every command but `up` talks to an engine over HTTP through [`vigily_api::client::Client`]:
//! the one at `--url` (`VIGILY_URL`), else the repository's local engine, started in the
//! background when none runs (see [`vigily_api::local::resolve`]). `up` runs `vigily-engine` in
//! the foreground. The commands are functions taking a client and the streams to write to, so
//! tests run them against an engine in the same process.

pub mod render;
pub mod sync;

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use vigily_api::auth::{CreateToken, Scope};
use vigily_api::client::{Client, Error};
use vigily_api::evals::ScheduleRunStatus;
use vigily_api::local::{self, Found, Resolve, ResolveError};
use vigily_api::runs::RunsQuery;
use vigily_api::traces::DeleteTraces;

use crate::render::{bytes, clip, count, duration, table, utc};

#[derive(Parser)]
#[command(
    name = "vigily",
    version,
    about = "An experiment tracker that keeps watch over runs, evaluations and drift"
)]
pub struct Cli {
    /// The repository: a directory, or an object-store URI such as s3://bucket/prefix.
    #[arg(
        long,
        short,
        global = true,
        env = "VIGILY_REPO",
        default_value = ".vigily"
    )]
    pub repo: String,
    /// An engine's address, such as a team server. Commands then use it instead of the
    /// repository's local engine, and never start one.
    #[arg(long, global = true, env = "VIGILY_URL")]
    pub url: Option<String>,
    /// The token to send. A local engine's own token is used when not given.
    #[arg(long, global = true, env = "VIGILY_TOKEN", hide_env_values = true)]
    pub token: Option<String>,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Serve the web app and the API in the foreground (runs vigily-engine).
    Up(UpArgs),
    /// Whether an engine serves the repository, and where.
    Status,
    /// Stop the repository's local engine.
    Stop,
    /// Print the repository's sign-in link and open it in a browser.
    Open {
        /// Only print the link.
        #[arg(long)]
        no_browser: bool,
    },
    /// Print VIGILY_URL and VIGILY_TOKEN for the repository's engine, as shell exports.
    Env,
    /// Send what the Python SDK left in its offline spool to this engine (--url, or the
    /// repository's). Batches for other engines are listed, not sent.
    Sync {
        /// Only list the spool directories that would be sent.
        #[arg(long)]
        dry_run: bool,
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
    /// Delete traces by id; with --erase, also erase them from storage (as `vigily runs purge`).
    Traces {
        #[command(subcommand)]
        action: TracesAction,
    },
    /// Compact storage and build the metrics index. Run it when training is done.
    Optimize,
    /// List the engine's schedules, or run one now.
    Schedules {
        #[command(subcommand)]
        action: Option<ScheduleAction>,
    },
    /// Access tokens of an engine in server mode.
    Tokens {
        #[command(subcommand)]
        action: TokensAction,
    },
    /// The local engine's own token.
    Token {
        #[command(subcommand)]
        action: TokenAction,
    },
}

/// The flags of `vigily up`, passed on to `vigily-engine`.
#[derive(clap::Args, Default)]
pub struct UpArgs {
    /// Address to listen on. Anything but a loopback address serves every host name.
    #[arg(long)]
    pub host: Option<String>,
    /// The port (default 7600; also VIGILY_PORT).
    #[arg(long, short)]
    pub port: Option<u16>,
    /// Do not optimize storage in the background (on by default).
    #[arg(long)]
    pub no_optimize: bool,
    /// How often to optimize storage in the background: `15m`, `1h`, ...
    #[arg(long)]
    pub optimize_every: Option<String>,
    /// Also optimize storage before serving (slower start-up; see docs/architecture.md).
    #[arg(long)]
    pub optimize_at_start: bool,
    /// The server configuration: schedules, alert webhook, drift thresholds. Defaults to
    /// ./vigily.toml when it exists (also VIGILY_CONFIG).
    #[arg(long)]
    pub config: Option<PathBuf>,
    /// `local` or `server`; local on a loopback address and server on any other by default.
    #[arg(long)]
    pub mode: Option<String>,
    /// Where the local token is kept (also VIGILY_TOKEN_FILE).
    #[arg(long)]
    pub token_file: Option<PathBuf>,
    /// Check no tokens at all, for an engine behind your own access control.
    #[arg(long)]
    pub insecure_no_auth: bool,
}

#[derive(Subcommand)]
pub enum TracesAction {
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
pub enum RunsAction {
    /// Erase deleted runs from storage for good: rewrite the files that still hold their data and
    /// remove older versions. Deleting in the web app only hides runs until this runs.
    Purge {
        /// Also delete storage tags that keep deleted data (they are named when the purge stops).
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
pub enum ScheduleAction {
    /// Run a schedule's command now on the engine, record it like a scheduled run, and follow
    /// it through its retries. Exits 1 unless it succeeds.
    Run { name: String },
}

#[derive(Subcommand)]
pub enum TokensAction {
    /// List tokens (never their secrets).
    List,
    /// Create a token; it is shown once.
    Create {
        /// A name such as ci or nightly-evals.
        name: String,
        /// read, ingest or admin; repeat for several (an SDK needs read and ingest).
        #[arg(long = "scope", required = true)]
        scopes: Vec<String>,
        /// Expire after this long: `30d`, `12h`.
        #[arg(long)]
        expires_in: Option<String>,
    },
    /// Revoke a token by id.
    Revoke { id: String },
}

#[derive(Subcommand)]
pub enum TokenAction {
    /// Replace the local engine's token: links and scripts using the old one stop working.
    Rotate,
}

/// Whether a command may start an engine for a repository that does not exist yet.
fn creates(c: &Command) -> bool {
    matches!(
        c,
        Command::Open { .. } | Command::Env | Command::Schedules { .. }
    )
}

/// A client for the repository's engine, started when needed and allowed.
pub async fn connect(cli: &Cli, start: bool) -> Result<(Client, Found)> {
    let r = Resolve {
        repo: cli.repo.clone(),
        url: cli.url.clone().filter(|u| !u.is_empty()),
        token: cli.token.clone().filter(|t| !t.is_empty()),
        start,
        ..Default::default()
    };
    local::resolve(&r).await.map_err(|e| match e {
        ResolveError::NotRunning { .. } if !exists(&cli.repo) => anyhow::anyhow!(
            "no repository at {}. Log a run with the Python SDK first, or pass --repo",
            cli.repo
        ),
        other => anyhow::anyhow!(other),
    })
}

/// Whether the repository exists (object-store URIs are assumed to).
fn exists(repo: &str) -> bool {
    repo.contains("://") || std::path::Path::new(repo).exists()
}

/// An API error as a command-line error, with a hint for a refused token.
pub fn explain(e: Error, c: &Client) -> anyhow::Error {
    match &e {
        Error::Api(a) if a.status == 401 => anyhow::anyhow!(
            "{}: {} needs a token; pass --token or set VIGILY_TOKEN",
            a.message,
            c.base()
        ),
        Error::Api(a) if a.status == 403 => anyhow::anyhow!(
            "{}: the token lacks the scope for this; an admin token can do it",
            a.message
        ),
        _ => anyhow::anyhow!(e),
    }
}

/// Runs a parsed command line.
pub async fn run(cli: Cli) -> Result<ExitCode> {
    let (mut out, mut err) = (std::io::stdout(), std::io::stderr());
    let (out, err): (&mut dyn Write, &mut dyn Write) = (&mut out, &mut err);
    match &cli.command {
        Command::Up(args) => return up(&cli.repo, args),
        Command::Status => return status(&cli, out).await,
        Command::Stop => return stop(&cli, out).await,
        Command::Sync { dry_run } => return sync_command(&cli, out, err, *dry_run).await,
        _ => {}
    }
    let start = exists(&cli.repo) || creates(&cli.command);
    let (c, _) = connect(&cli, start).await?;
    let code = match &cli.command {
        Command::Up(_) | Command::Status | Command::Stop | Command::Sync { .. } => unreachable!(),
        Command::Open { no_browser } => open(&c, out, !no_browser).await,
        Command::Env => env(&c, out),
        Command::Runs {
            action: None,
            query,
            sort,
            desc,
            limit,
        } => runs(&c, out, err, query, sort.as_deref(), *desc, *limit).await,
        Command::Runs {
            action: Some(RunsAction::Purge { force }),
            ..
        } => purge(&c, out, *force).await,
        Command::Traces {
            action: TracesAction::Delete { ids, erase, force },
        } => delete_traces(&c, out, err, ids, *erase, *force).await,
        Command::Optimize => optimize(&c, out, &cli.repo).await,
        Command::Schedules { action: None } => schedules(&c, out).await,
        Command::Schedules {
            action: Some(ScheduleAction::Run { name }),
        } => run_schedule(&c, out, name).await,
        Command::Tokens { action } => match action {
            TokensAction::List => tokens(&c, out).await,
            TokensAction::Create {
                name,
                scopes,
                expires_in,
            } => create_token(&c, out, name, scopes, expires_in.as_deref()).await,
            TokensAction::Revoke { id } => revoke_token(&c, out, id).await,
        },
        Command::Token {
            action: TokenAction::Rotate,
        } => rotate_token(&c, out).await,
    };
    code
}

/// `vigily sync`: connects (starting the repository's engine if needed) only when the spool holds
/// something for it.
async fn sync_command(
    cli: &Cli,
    out: &mut dyn Write,
    err: &mut dyn Write,
    dry_run: bool,
) -> Result<ExitCode> {
    let target = match cli.url.as_deref().filter(|u| !u.is_empty()) {
        Some(u) => sync::Target::Url(u.trim_end_matches('/').to_string()),
        None => sync::Target::Repo(local::canonical_location(&cli.repo)),
    };
    let Some(root) = sync::spool_root() else {
        bail!("no home directory for vigily's state; set VIGILY_STATE_DIR");
    };
    if !sync::has_work(&root, &target) {
        writeln!(out, "Nothing in the spool for this engine.")?;
        return sync::sync(
            &Client::new("http://127.0.0.1:0", None),
            &target,
            &root,
            true,
            out,
            err,
        )
        .await
        .map(|_| ExitCode::SUCCESS);
    }
    let (c, _) = connect(cli, true).await?;
    sync::sync(&c, &target, &root, dry_run, out, err).await
}

/// Runs `vigily-engine` in the foreground with the flags of `vigily up`; on Unix it replaces this
/// process, so signals and the exit code are the engine's.
pub fn up(repo: &str, a: &UpArgs) -> Result<ExitCode> {
    let bin = local::engine_binary().ok_or_else(|| anyhow::anyhow!(ResolveError::NoEngine))?;
    let mut cmd = std::process::Command::new(&bin);
    cmd.arg("--repo").arg(repo);
    if let Some(h) = &a.host {
        cmd.args(["--host", h]);
    }
    if let Some(p) = a.port {
        cmd.args(["--port", &p.to_string()]);
    }
    if a.no_optimize {
        cmd.arg("--no-optimize");
    }
    if let Some(e) = &a.optimize_every {
        cmd.args(["--optimize-every", e]);
    }
    if a.optimize_at_start {
        cmd.arg("--optimize-at-start");
    }
    if let Some(c) = &a.config {
        cmd.arg("--config").arg(c);
    }
    if let Some(m) = &a.mode {
        cmd.args(["--mode", m]);
    }
    if let Some(t) = &a.token_file {
        cmd.arg("--token-file").arg(t);
    }
    if a.insecure_no_auth {
        cmd.arg("--insecure-no-auth");
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let e = cmd.exec();
        bail!("could not run {}: {e}", bin.display())
    }
    #[cfg(not(unix))]
    {
        let status = cmd
            .status()
            .with_context(|| format!("could not run {}", bin.display()))?;
        Ok(ExitCode::from(
            status.code().unwrap_or(1).clamp(0, 255) as u8
        ))
    }
}

/// `vigily status`: the engine's address, version and mode; exit 1 when none serves the
/// repository. Never starts one.
pub async fn status(cli: &Cli, out: &mut dyn Write) -> Result<ExitCode> {
    let (c, found) = match connect(cli, false).await {
        Ok(x) => x,
        Err(e) => {
            writeln!(out, "{e}")?;
            return Ok(ExitCode::FAILURE);
        }
    };
    let info = c.info().await.map_err(|e| explain(e, &c))?;
    writeln!(out, "engine   {}", c.base())?;
    writeln!(out, "version  {} (API {})", info.version, info.api_version)?;
    writeln!(out, "mode     {}", info.mode)?;
    match &found {
        Found::Url => writeln!(out, "from     VIGILY_URL")?,
        Found::Running(f) | Found::Started(f) => {
            writeln!(out, "repo     {}", f.repo)?;
            writeln!(out, "pid      {}", f.pid)?;
            writeln!(
                out,
                "started  {}{}",
                utc(f.started_at),
                if f.auto_started {
                    " in the background; stops when idle"
                } else {
                    ""
                }
            )?;
        }
    }
    match c.me().await {
        Ok(me) if me.authenticated => writeln!(
            out,
            "token    {} ({})",
            me.principal.unwrap_or_default(),
            me.scopes
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )?,
        Ok(me) if me.auth_required => writeln!(out, "token    none or not accepted")?,
        _ => {}
    }
    Ok(ExitCode::SUCCESS)
}

/// `vigily stop`: stops the repository's local engine and waits until it is gone.
pub async fn stop(cli: &Cli, out: &mut dyn Write) -> Result<ExitCode> {
    if cli.url.as_deref().is_some_and(|u| !u.is_empty()) {
        bail!("vigily stop stops the local engine of a repository; VIGILY_URL names another one");
    }
    let found = match connect(cli, false).await {
        Ok((_, Found::Running(f))) => f,
        _ => {
            writeln!(out, "No engine serves {}.", cli.repo)?;
            return Ok(ExitCode::SUCCESS);
        }
    };
    terminate(found.pid)?;
    let paths = local::EnginePaths::new(
        &local::state_dir().context("no state directory")?,
        &found.repo,
    );
    for _ in 0..300 {
        if paths.read().is_none_or(|f| f.pid != found.pid) {
            writeln!(
                out,
                "Stopped the engine at {} (pid {}).",
                found.url, found.pid
            )?;
            return Ok(ExitCode::SUCCESS);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    bail!(
        "the engine (pid {}) did not stop within 30 seconds",
        found.pid
    )
}

/// Asks process `pid` to stop (SIGTERM on Unix).
fn terminate(pid: u32) -> Result<()> {
    let ok = if cfg!(windows) {
        std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string()])
            .status()
    } else {
        std::process::Command::new("kill")
            .arg(pid.to_string())
            .status()
    }
    .is_ok_and(|s| s.success());
    anyhow::ensure!(ok, "could not stop the engine (pid {pid})");
    Ok(())
}

/// The link that signs a browser in: the engine's address, with the local token.
pub fn sign_in_link(c: &Client) -> String {
    match c.token() {
        Some(t) if !t.starts_with("vgy_") => format!("{}/?token={t}", c.base()),
        _ => c.base().to_string(),
    }
}

/// `vigily open`.
pub async fn open(c: &Client, out: &mut dyn Write, browser: bool) -> Result<ExitCode> {
    let link = sign_in_link(c);
    writeln!(out, "{link}")?;
    if browser {
        let opener = if cfg!(target_os = "macos") {
            Some("open")
        } else if std::env::var_os("DISPLAY").is_some()
            || std::env::var_os("WAYLAND_DISPLAY").is_some()
        {
            Some("xdg-open")
        } else {
            None
        };
        if let Some(o) = opener {
            let _ = std::process::Command::new(o)
                .arg(&link)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// `vigily env`: shell exports for scripts and the SDK.
pub fn env(c: &Client, out: &mut dyn Write) -> Result<ExitCode> {
    writeln!(out, "export VIGILY_URL={}", c.base())?;
    if let Some(t) = c.token() {
        writeln!(out, "export VIGILY_TOKEN={t}")?;
    }
    Ok(ExitCode::SUCCESS)
}

/// `vigily runs`: a table of runs, newest first (or as sorted).
pub async fn runs(
    c: &Client,
    out: &mut dyn Write,
    err: &mut dyn Write,
    query: &str,
    sort: Option<&str>,
    desc: bool,
    limit: usize,
) -> Result<ExitCode> {
    let r = c
        .run_summaries(&RunsQuery {
            q: query.to_string(),
            sort: sort.map(str::to_string),
            desc,
            limit: Some(limit.clamp(1, 10_000)),
            ..Default::default()
        })
        .await
        .map_err(|e| match e {
            Error::Api(a) if a.code == vigily_api::code::INVALID_ARGUMENT => {
                anyhow::anyhow!("{}", a.message)
            }
            e => explain(e, c),
        })?;
    writeln!(
        out,
        "{:<10} {:<28} {:<16} {:<9} {:>10}",
        "ID", "NAME", "EXPERIMENT", "STATUS", "DURATION"
    )?;
    for r in r.rows.iter().take(limit) {
        let n = r.id.chars().count();
        let short: String = r.id.chars().skip(n.saturating_sub(8)).collect();
        writeln!(
            out,
            "{:<10} {:<28} {:<16} {:<9} {:>10}",
            short,
            clip(&r.name, 28),
            clip(r.experiment.as_deref().unwrap_or("-"), 16),
            r.status.as_str(),
            duration(r.duration),
        )?;
    }
    if r.matched > limit {
        writeln!(err, "{} more; raise --limit to see them", r.matched - limit)?;
    }
    Ok(ExitCode::SUCCESS)
}

/// `vigily runs purge`.
pub async fn purge(c: &Client, out: &mut dyn Write, force: bool) -> Result<ExitCode> {
    let r = c.purge(force).await.map_err(|e| explain(e, c))?;
    writeln!(
        out,
        "Purged {} deleted {}: {} files rewritten, {} old versions and {} removed.",
        r.runs,
        if r.runs == 1 { "run" } else { "runs" },
        r.fragments_rewritten,
        r.versions_removed,
        bytes(r.bytes_removed)
    )?;
    if !r.tags_deleted.is_empty() {
        writeln!(
            out,
            "Tags deleted because they kept erased data: {}",
            r.tags_deleted.join(", ")
        )?;
    }
    if r.legacy_records_present {
        writeln!(
            out,
            "Note: records.lance is the records table of an earlier version, already migrated. Purge does not \
             reach it: it still holds every record as of the upgrade. Delete it by hand to erase them."
        )?;
    }
    writeln!(
        out,
        "Not reached by a purge: backups, object-store versioning or replication, other machines' \
         write-ahead logs of crashed writers, and disk blocks (see the storage guide)."
    )?;
    Ok(ExitCode::SUCCESS)
}

/// `vigily traces delete`.
pub async fn delete_traces(
    c: &Client,
    out: &mut dyn Write,
    err: &mut dyn Write,
    ids: &[String],
    erase: bool,
    force: bool,
) -> Result<ExitCode> {
    let r = c
        .delete_traces(&DeleteTraces {
            ids: ids.to_vec(),
            erase,
            force,
        })
        .await
        .map_err(|e| explain(e, c))?;
    for id in &r.missing {
        writeln!(err, "No trace with id {id}.")?;
    }
    writeln!(out, "Deleted {}.", count(r.deleted, "trace", "traces"))?;
    match &r.purge {
        Some(p) => writeln!(
            out,
            "Erased: {} files rewritten, {} old versions and {} removed. Not reached: backups, \
             object-store versioning or replication, and disk blocks (see the storage guide).",
            p.fragments_rewritten,
            p.versions_removed,
            bytes(p.bytes_removed)
        )?,
        None => writeln!(
            out,
            "Deleting hides them; `vigily runs purge` (or --erase) erases them from storage."
        )?,
    }
    Ok(ExitCode::SUCCESS)
}

/// `vigily optimize`.
pub async fn optimize(c: &Client, out: &mut dyn Write, repo: &str) -> Result<ExitCode> {
    let r = c.optimize().await.map_err(|e| explain(e, c))?;
    writeln!(out, "Optimized {repo} in {:.1} s", r.seconds)?;
    Ok(ExitCode::SUCCESS)
}

/// `vigily schedules`: the engine's schedules with their next fire time.
pub async fn schedules(c: &Client, out: &mut dyn Write) -> Result<ExitCode> {
    let r = c.schedules().await.map_err(|e| explain(e, c))?;
    if r.schedules.is_empty() {
        writeln!(
            out,
            "No schedules{}.",
            r.config_path
                .map(|p| format!(" in {p}"))
                .unwrap_or_else(|| ": the engine has no configuration file".into())
        )?;
        return Ok(ExitCode::SUCCESS);
    }
    let rows: Vec<Vec<String>> = r
        .schedules
        .iter()
        .map(|s| {
            vec![
                s.name.clone(),
                s.cron.clone(),
                s.timezone.clone(),
                if s.enabled {
                    s.next_run.map(utc).unwrap_or_else(|| "-".into())
                } else {
                    "disabled".into()
                },
                s.command.join(" "),
            ]
        })
        .collect();
    write!(
        out,
        "{}",
        table(&["NAME", "CRON", "TIMEZONE", "NEXT", "COMMAND"], &rows)
    )?;
    Ok(ExitCode::SUCCESS)
}

/// `vigily schedules run NAME`: starts the run on the engine and follows it through its
/// retries; exit 1 unless the last attempt succeeded.
pub async fn run_schedule(c: &Client, out: &mut dyn Write, name: &str) -> Result<ExitCode> {
    let started = c
        .start_schedule_run(name)
        .await
        .map_err(|e| explain(e, c))?;
    writeln!(out, "Started {name} (run {}).", started.id)?;
    let mut lines = Vec::new();
    let last = c
        .follow_schedule_run(name, &started.id, |a| {
            lines.push(format!(
                "attempt {}: {}{}",
                a.run.attempt,
                status_text(a.run.status),
                match (a.run.exit_code, &a.run.reason) {
                    (Some(code), _) => format!(", exit {code}"),
                    (None, Some(r)) => format!(", {r}"),
                    (None, None) => String::new(),
                }
            ))
        })
        .await
        .map_err(|e| explain(e, c))?;
    for l in lines {
        writeln!(out, "{l}")?;
    }
    Ok(if last.run.status == ScheduleRunStatus::Succeeded {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn status_text(s: ScheduleRunStatus) -> &'static str {
    match s {
        ScheduleRunStatus::Running => "running",
        ScheduleRunStatus::Succeeded => "succeeded",
        ScheduleRunStatus::Failed => "failed",
        ScheduleRunStatus::TimedOut => "timed out",
        ScheduleRunStatus::SkippedOverlap => "skipped (overlap)",
        ScheduleRunStatus::SkippedMissed => "skipped (missed)",
        ScheduleRunStatus::Interrupted => "interrupted",
    }
}

/// `vigily tokens list`.
pub async fn tokens(c: &Client, out: &mut dyn Write) -> Result<ExitCode> {
    let r = c.tokens().await.map_err(|e| explain(e, c))?;
    if r.tokens.is_empty() {
        writeln!(out, "No tokens.")?;
        return Ok(ExitCode::SUCCESS);
    }
    let rows: Vec<Vec<String>> = r
        .tokens
        .iter()
        .map(|t| {
            vec![
                t.id.clone(),
                t.name.clone(),
                t.scopes
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(","),
                utc(t.created_at),
                t.last_used_at.map(utc).unwrap_or_else(|| "never".into()),
                t.expires_at.map(utc).unwrap_or_else(|| "never".into()),
            ]
        })
        .collect();
    write!(
        out,
        "{}",
        table(
            &["ID", "NAME", "SCOPES", "CREATED", "LAST USED", "EXPIRES"],
            &rows
        )
    )?;
    Ok(ExitCode::SUCCESS)
}

/// A duration such as `30d` or `12h`, in microseconds.
fn parse_micros(s: &str) -> Result<i64> {
    let s = s.trim();
    let i = s.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(s.len());
    let n: f64 = s[..i]
        .trim()
        .parse()
        .with_context(|| format!("{s:?} is not a duration such as 12h or 30d"))?;
    let secs = match &s[i..] {
        "s" | "" => n,
        "m" => n * 60.0,
        "h" => n * 3600.0,
        "d" => n * 86_400.0,
        _ => bail!("{s:?}: use s, m, h or d"),
    };
    anyhow::ensure!(secs > 0.0, "{s:?} must be positive");
    Ok((secs * 1e6) as i64)
}

fn now_micros() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0)
}

/// `vigily tokens create`.
pub async fn create_token(
    c: &Client,
    out: &mut dyn Write,
    name: &str,
    scopes: &[String],
    expires_in: Option<&str>,
) -> Result<ExitCode> {
    let scopes = scopes
        .iter()
        .flat_map(|s| s.split(','))
        .map(|s| {
            serde_json::from_value::<Scope>(serde_json::Value::String(s.trim().to_lowercase()))
                .map_err(|_| anyhow::anyhow!("unknown scope {s:?}: use read, ingest or admin"))
        })
        .collect::<Result<Vec<_>>>()?;
    let expires_at = match expires_in {
        Some(e) => Some(now_micros() + parse_micros(e)?),
        None => None,
    };
    let r = c
        .create_token(&CreateToken {
            name: name.to_string(),
            scopes,
            expires_at,
        })
        .await
        .map_err(|e| explain(e, c))?;
    writeln!(out, "{}", r.token)?;
    writeln!(
        out,
        "Token {} ({}) created. It is shown only now; keep it somewhere safe.",
        r.info.id, r.info.name
    )?;
    Ok(ExitCode::SUCCESS)
}

/// `vigily tokens revoke`.
pub async fn revoke_token(c: &Client, out: &mut dyn Write, id: &str) -> Result<ExitCode> {
    c.revoke_token(id).await.map_err(|e| explain(e, c))?;
    writeln!(out, "Revoked token {id}.")?;
    Ok(ExitCode::SUCCESS)
}

/// `vigily token rotate`.
pub async fn rotate_token(c: &Client, out: &mut dyn Write) -> Result<ExitCode> {
    let r = c.rotate_local_token().await.map_err(|e| explain(e, c))?;
    let fresh = Client::new(c.base(), Some(r.token));
    writeln!(
        out,
        "Rotated the local token; links and scripts with the old one no longer work. Sign in again:"
    )?;
    writeln!(out, "{}", sign_in_link(&fresh))?;
    Ok(ExitCode::SUCCESS)
}
