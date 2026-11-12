//! The commands against an engine in this process: tables, messages and exit codes. A few run
//! the `vigily` binary itself, pointed at that engine with `--url`.

#![recursion_limit = "256"]

use std::collections::HashMap;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use vigily_api::client::Client;
use vigily_core::{
    Error as CoreError, MemoryStore, NewRun, Points, PurgeSummary, Result as CoreResult, Run,
    RunPatch, RunWriter, Series, StepRange, Store,
};
use vigily_server::config::{Config, ScheduleConfig};
use vigily_server::{serve, AppState, AuthSetup, ServeOptions};

const VIGILY: &str = env!("CARGO_BIN_EXE_vigily");

/// An engine over `store` in local mode on a free port; its URL and token.
async fn engine_over(dir: &Path, store: Arc<dyn Store>, config: Config) -> (String, String) {
    let records = vigily_core::open_records("memory").await.unwrap();
    let state = AppState::with(store, records, config);
    let (tx, rx) = tokio::sync::oneshot::channel();
    let opts = ServeOptions {
        addr: "127.0.0.1:0".parse().unwrap(),
        local_only: true,
        optimize: false,
        optimize_every: Duration::from_secs(3600),
        optimize_at_start: false,
        auth: AuthSetup::Local {
            token_file: dir.join("token"),
        },
        port_fallback: false,
        stop: None,
    };
    tokio::spawn(serve(state, opts, move |r| {
        let _ = tx.send((r.url.clone(), r.token.clone().unwrap()));
    }));
    rx.await.unwrap()
}

async fn engine(dir: &Path, config: Config) -> Client {
    let (url, token) = engine_over(dir, Arc::new(MemoryStore::new()), config).await;
    Client::new(&url, Some(token))
}

/// Logs runs over the API: `(id, name, experiment)`, the last one still running.
async fn log_runs(c: &Client, runs: &[(&str, &str, &str)]) {
    let mut ops = Vec::new();
    for (i, (id, name, exp)) in runs.iter().enumerate() {
        ops.push(
            json!({"op": "create", "id": id, "name": name, "experiment": exp,
                        "created_at": 1_700_000_000_000_000i64 + i as i64 * 1_000_000}),
        );
        if i + 1 < runs.len() {
            ops.push(json!({"op": "finish", "run_id": id, "status": "finished",
                            "ended_at": 1_700_000_060_000_000i64 + i as i64 * 1_000_000}));
        }
    }
    let batch = serde_json::from_value(
        json!({"writer": "0190a0b0c0d07e0f8a9b0c1d2e3f4a60", "seq": 1, "ops": ops}),
    )
    .unwrap();
    c.ingest(&batch, None).await.unwrap();
    c.flush().await.unwrap();
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).to_string()
}

const A: &str = "0190a0b0c0d07e0f8a9b0c1d2e3f4a01";
const B: &str = "0190a0b0c0d07e0f8a9b0c1d2e3f4a02";
const C: &str = "0190a0b0c0d07e0f8a9b0c1d2e3f4a03";

#[tokio::test]
async fn runs_prints_a_table_and_how_many_more_match() {
    let dir = tempfile::tempdir().unwrap();
    let c = engine(dir.path(), Config::default()).await;
    log_runs(
        &c,
        &[
            (A, "baseline", "mnist"),
            (B, "a-run-with-a-name-longer-than-the-column", "mnist"),
            (C, "live", "cifar10"),
        ],
    )
    .await;

    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = vigily_cli::runs(&c, &mut out, &mut err, "", None, false, 2)
        .await
        .unwrap();
    assert_eq!(code, ExitCode::SUCCESS);
    let out = text(&out);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 3, "{out}");
    assert_eq!(
        lines[0],
        "ID         NAME                         EXPERIMENT       STATUS      DURATION"
    );
    assert!(lines[1].starts_with("2e3f4a03   live "), "{out}");
    assert!(lines[1].contains(" running "), "{out}");
    assert!(
        lines[2].contains("a-run-with-a-name-longer-th… mnist "),
        "{out}"
    );
    assert!(lines[2].ends_with("1m 00s"), "{out}");
    assert_eq!(text(&err), "1 more; raise --limit to see them\n");

    let (mut out, mut err) = (Vec::new(), Vec::new());
    vigily_cli::runs(
        &c,
        &mut out,
        &mut err,
        "exp:mnist",
        Some("run:name"),
        false,
        50,
    )
    .await
    .unwrap();
    let out = text(&out);
    let names: Vec<&str> = out
        .lines()
        .skip(1)
        .map(|l| l.split_whitespace().nth(1).unwrap())
        .collect();
    assert_eq!(names, ["a-run-with-a-name-longer-th…", "baseline"]);
    assert!(err.is_empty());

    let e = vigily_cli::runs(&c, &mut Vec::new(), &mut Vec::new(), "lr<", None, false, 5)
        .await
        .unwrap_err();
    assert!(e.to_string().contains("needs a value"), "{e}");
}

/// A store whose purge is refused because of a storage tag, unless forced.
struct Tagged(MemoryStore);

#[async_trait]
impl Store for Tagged {
    fn location(&self) -> String {
        self.0.location()
    }
    async fn runs(&self) -> CoreResult<Vec<Run>> {
        self.0.runs().await
    }
    async fn params(&self, run_id: &str) -> CoreResult<Value> {
        self.0.params(run_id).await
    }
    async fn series(&self, run_id: &str) -> CoreResult<Vec<Series>> {
        self.0.series(run_id).await
    }
    async fn points(&self, ids: &[u64], range: StepRange) -> CoreResult<HashMap<u64, Points>> {
        self.0.points(ids, range).await
    }
    async fn delete_runs(&self, run_ids: &[String]) -> CoreResult<usize> {
        self.0.delete_runs(run_ids).await
    }
    async fn purge(&self, force: bool) -> CoreResult<PurgeSummary> {
        if force {
            Ok(PurgeSummary {
                runs: 1,
                fragments_rewritten: 2,
                versions_removed: 3,
                bytes_removed: 1_500_000,
                tags_deleted: vec!["before-cleanup".into()],
                ..Default::default()
            })
        } else {
            Err(CoreError::Storage(
                "tag before-cleanup keeps a version with deleted rows".into(),
            ))
        }
    }
    async fn create_run(&self, new: NewRun) -> CoreResult<Box<dyn RunWriter>> {
        self.0.create_run(new).await
    }
    async fn open_run(&self, run_id: &str) -> CoreResult<Box<dyn RunWriter>> {
        self.0.open_run(run_id).await
    }
    async fn update_run(&self, run_id: &str, patch: RunPatch) -> CoreResult<Run> {
        self.0.update_run(run_id, patch).await
    }
    async fn flush(&self) -> CoreResult<()> {
        self.0.flush().await
    }
    async fn close(&self) -> CoreResult<()> {
        self.0.close().await
    }
}

#[tokio::test]
async fn purge_names_a_blocking_tag_and_force_deletes_it() {
    let dir = tempfile::tempdir().unwrap();
    let (url, token) = engine_over(
        dir.path(),
        Arc::new(Tagged(MemoryStore::new())),
        Config::default(),
    )
    .await;
    let c = Client::new(&url, Some(token.clone()));
    let e = vigily_cli::purge(&c, &mut Vec::new(), false)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        e.starts_with("storage error: tag before-cleanup keeps a version"),
        "{e}"
    );
    assert!(e.contains("Run `vigily runs purge --force`"), "{e}");

    let mut out = Vec::new();
    vigily_cli::purge(&c, &mut out, true).await.unwrap();
    let out = text(&out);
    assert!(
        out.starts_with(
            "Purged 1 deleted run: 2 files rewritten, 3 old versions and 1.5 MB removed.\n\
             Tags deleted because they kept erased data: before-cleanup\n"
        ),
        "{out}"
    );

    // The binary: the same message on stderr, exit code 1.
    let bin = tokio::process::Command::new(VIGILY)
        .args(["--url", &url, "--token", &token, "runs", "purge"])
        .env_remove("VIGILY_REPO")
        .output()
        .await
        .unwrap();
    assert_eq!(bin.status.code(), Some(1));
    let err = text(&bin.stderr);
    assert!(
        err.starts_with("error: storage error: tag before-cleanup"),
        "{err}"
    );
}

fn schedule(dir: &Path, name: &str, cmd: &str, retries: u32) -> ScheduleConfig {
    ScheduleConfig {
        name: name.into(),
        description: None,
        cron: "0 2 1 1 *".into(),
        timezone: "UTC".into(),
        command: vec!["sh".into(), "-c".into(), cmd.into()],
        workdir: Some(dir.to_path_buf()),
        env: Default::default(),
        timeout: "30s".into(),
        retries,
        retry_delay: "0s".into(),
        missed: Default::default(),
        enabled: true,
        keep_runs: 200,
        keep_days: 90,
        keep_output: 50,
    }
}

#[tokio::test]
async fn schedules_list_and_a_failing_run_exits_1() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config {
        schedule: vec![
            schedule(dir.path(), "nightly", "echo ok", 0),
            schedule(dir.path(), "flaky", "exit 3", 1),
        ],
        ..Default::default()
    };
    let c = engine(dir.path(), config).await;

    let mut out = Vec::new();
    vigily_cli::schedules(&c, &mut out).await.unwrap();
    let out = text(&out);
    let lines: Vec<&str> = out.lines().collect();
    assert!(
        lines[0].starts_with("NAME     CRON       TIMEZONE  NEXT "),
        "{out}"
    );
    assert!(
        lines[1].starts_with("nightly  0 2 1 1 *  UTC       "),
        "{out}"
    );
    assert!(lines[1].ends_with("  sh -c echo ok"), "{out}");

    let mut out = Vec::new();
    let code = vigily_cli::run_schedule(&c, &mut out, "nightly")
        .await
        .unwrap();
    assert_eq!(code, ExitCode::SUCCESS);
    assert!(text(&out).ends_with("attempt 1: succeeded, exit 0\n"));

    let mut out = Vec::new();
    let code = vigily_cli::run_schedule(&c, &mut out, "flaky")
        .await
        .unwrap();
    assert_eq!(code, ExitCode::FAILURE);
    let out = text(&out);
    assert!(
        out.ends_with("attempt 1: failed, exit 3\nattempt 2: failed, exit 3\n"),
        "{out}"
    );

    let e = vigily_cli::run_schedule(&c, &mut Vec::new(), "nope")
        .await
        .unwrap_err();
    assert!(e.to_string().contains("no schedule named nope"), "{e}");
}

#[tokio::test]
async fn tokens_traces_optimize_env_and_rotation() {
    let dir = tempfile::tempdir().unwrap();
    let c = engine(dir.path(), Config::default()).await;

    let mut out = Vec::new();
    vigily_cli::tokens(&c, &mut out).await.unwrap();
    assert_eq!(text(&out), "No tokens.\n");
    let mut out = Vec::new();
    vigily_cli::create_token(&c, &mut out, "ci", &["read,ingest".into()], Some("30d"))
        .await
        .unwrap();
    let out = text(&out);
    let token = out.lines().next().unwrap().to_string();
    assert!(token.starts_with("vgy_"), "{out}");
    let id = token.split('_').nth(1).unwrap().to_string();
    let mut out = Vec::new();
    vigily_cli::tokens(&c, &mut out).await.unwrap();
    let out = text(&out);
    assert!(out.starts_with("ID  "), "{out}");
    assert!(out.contains(&format!("{id}  ci    read,ingest  ")), "{out}");
    assert!(
        vigily_cli::create_token(&c, &mut Vec::new(), "x", &["write".into()], None)
            .await
            .unwrap_err()
            .to_string()
            .contains("unknown scope")
    );
    let mut out = Vec::new();
    vigily_cli::revoke_token(&c, &mut out, &id).await.unwrap();
    assert_eq!(text(&out), format!("Revoked token {id}.\n"));

    let (mut out, mut err) = (Vec::new(), Vec::new());
    vigily_cli::delete_traces(&c, &mut out, &mut err, &["missing".into()], false, false)
        .await
        .unwrap();
    assert_eq!(text(&err), "No trace with id missing.\n");
    assert!(text(&out).starts_with("Deleted 0 traces.\nDeleting hides them"));

    let mut out = Vec::new();
    vigily_cli::optimize(&c, &mut out, "repo").await.unwrap();
    assert!(text(&out).starts_with("Optimized repo in "));

    let mut out = Vec::new();
    vigily_cli::env(&c, &mut out).unwrap();
    assert_eq!(
        text(&out),
        format!(
            "export VIGILY_URL={}\nexport VIGILY_TOKEN={}\n",
            c.base(),
            c.token().unwrap()
        )
    );
    let mut out = Vec::new();
    vigily_cli::open(&c, &mut out, false).await.unwrap();
    assert_eq!(
        text(&out),
        format!("{}/?token={}\n", c.base(), c.token().unwrap())
    );

    let mut out = Vec::new();
    vigily_cli::rotate_token(&c, &mut out).await.unwrap();
    let out = text(&out);
    let link = out.lines().last().unwrap();
    let fresh = link.split("token=").nth(1).unwrap();
    assert_ne!(fresh, c.token().unwrap());
    assert_eq!(
        std::fs::read_to_string(dir.path().join("token"))
            .unwrap()
            .trim(),
        fresh
    );
    // The old token no longer opens the API; the new one does.
    assert_eq!(c.meta().await.unwrap_err().status(), Some(401));
    let renewed = Client::new(c.base(), Some(fresh.to_string()));
    assert!(renewed.meta().await.is_ok());
}

#[tokio::test]
async fn status_names_the_engine_and_read_commands_need_a_repository() {
    let dir = tempfile::tempdir().unwrap();
    let c = engine(dir.path(), Config::default()).await;
    let bin = tokio::process::Command::new(VIGILY)
        .args(["--url", c.base(), "--token", c.token().unwrap(), "status"])
        .output()
        .await
        .unwrap();
    assert!(bin.status.success(), "{}", text(&bin.stderr));
    let out = text(&bin.stdout);
    assert!(
        out.starts_with(&format!("engine   {}\nversion  ", c.base())),
        "{out}"
    );
    assert!(out.contains("mode     local\n"), "{out}");
    assert!(out.contains("\ntoken    "), "{out}");

    // Without an engine or a repository, a read command says so and starts nothing.
    let state = dir.path().join("state");
    let missing = dir.path().join("missing");
    let bin = tokio::process::Command::new(VIGILY)
        .arg("--repo")
        .arg(&missing)
        .arg("runs")
        .env("VIGILY_STATE_DIR", &state)
        .env_remove("VIGILY_URL")
        .env_remove("VIGILY_TOKEN")
        .output()
        .await
        .unwrap();
    assert_eq!(bin.status.code(), Some(1));
    assert_eq!(
        text(&bin.stderr),
        format!(
            "error: no repository at {}. Log a run with the Python SDK first, or pass --repo\n",
            missing.display()
        )
    );
    assert!(!state.join("engines").exists());

    let bin = tokio::process::Command::new(VIGILY)
        .arg("--repo")
        .arg(&missing)
        .arg("status")
        .env("VIGILY_STATE_DIR", &state)
        .env_remove("VIGILY_URL")
        .output()
        .await
        .unwrap();
    assert_eq!(bin.status.code(), Some(1));
    assert!(text(&bin.stdout).starts_with("no repository at "));
}
