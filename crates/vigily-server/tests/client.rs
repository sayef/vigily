//! The typed client (`vigily_api::client`) against an engine in this process, and finding or
//! starting a repository's engine (`vigily_api::local::resolve`) with the real binary.

#![recursion_limit = "256"]

use std::path::Path;
use std::time::Duration;

use serde_json::json;
use vigily_api::client::{Client, Error};
use vigily_api::evals::ScheduleRunStatus;
use vigily_api::local::{resolve, Found, Resolve, ResolveError};
use vigily_api::runs::{PointsQuery, RunsQuery};
use vigily_server::config::{Config, ScheduleConfig};
use vigily_server::{serve, AppState, AuthSetup, ServeOptions};

const RUN: &str = "0190a0b0c0d07e0f8a9b0c1d2e3f4a5b";
const ENGINE: &str = env!("CARGO_BIN_EXE_vigily-engine");

fn schedule(dir: &Path, name: &str, cmd: &[&str], retries: u32) -> ScheduleConfig {
    ScheduleConfig {
        name: name.into(),
        description: None,
        cron: "0 2 1 1 *".into(),
        timezone: "UTC".into(),
        command: cmd.iter().map(|s| s.to_string()).collect(),
        workdir: Some(dir.to_path_buf()),
        env: Default::default(),
        timeout: "30s".into(),
        retries,
        retry_delay: "0s".into(),
        missed: Default::default(),
        enabled: false,
        keep_runs: 200,
        keep_days: 90,
        keep_output: 50,
    }
}

/// An engine over a repository in `dir`, on a free port, in local mode; its URL and token.
async fn engine(dir: &Path, config: Config) -> (String, String) {
    let repo = dir.join("repo");
    let repo = repo.to_str().unwrap();
    let store = vigily_core::open_store(repo).await.unwrap();
    let records = vigily_core::open_records(repo).await.unwrap();
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

fn api(e: Error) -> vigily_api::client::ApiError {
    match e {
        Error::Api(a) => a,
        other => panic!("expected an API error, got {other}"),
    }
}

#[tokio::test]
async fn the_client_writes_reads_and_names_errors() {
    let dir = tempfile::tempdir().unwrap();
    let (url, token) = engine(dir.path(), Config::default()).await;
    let c = Client::new(&url, Some(token.clone()));

    assert_eq!(c.health().await.unwrap(), "ok");
    let info = c.info().await.unwrap();
    assert_eq!(
        (info.api_version.as_str(), info.mode.as_str()),
        ("v1", "local")
    );
    assert!(c.me().await.unwrap().authenticated);

    // 20 000 points: the body is over 64 KiB and goes gzip-compressed.
    let n = 20_000;
    let batch = serde_json::from_value(json!({
        "writer": "0190a0b0c0d07e0f8a9b0c1d2e3f4a60",
        "seq": 1,
        "ops": [
            {"op": "create", "id": RUN, "name": "baseline", "experiment": "mnist"},
            {"op": "params", "run_id": RUN, "params": {"lr": 0.1}},
            {"op": "points", "run_id": RUN, "name": "loss", "context": {},
             "steps": (0..n).collect::<Vec<i64>>(),
             "values": (0..n).map(|i| 1.0 / (i as f64 + 1.0)).collect::<Vec<f64>>()},
        ]
    }))
    .unwrap();
    assert!(serde_json::to_vec(&batch).unwrap().len() > vigily_api::client::GZIP_ABOVE);
    let report = c.ingest(&batch, Some("batch-1")).await.unwrap();
    assert_eq!(report.seq, 1);
    c.flush().await.unwrap();

    let runs = c.run_summaries(&RunsQuery::default()).await.unwrap();
    assert_eq!((runs.total, runs.matched), (1, 1));
    assert_eq!(runs.rows[0].name, "baseline");
    let full = c.runs(&RunsQuery::default()).await.unwrap();
    assert_eq!(full.rows[0].params["lr"], json!(0.1));
    assert_eq!(c.run(RUN).await.unwrap().row.run.name, "baseline");
    assert_eq!(c.run_params(RUN).await.unwrap().params["lr"], json!(0.1));
    assert_eq!(c.run_series(RUN).await.unwrap().series.len(), 1);
    let points = c
        .run_points(
            RUN,
            &PointsQuery {
                name: "loss".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(points.steps.len(), n as usize);
    let meta = c.meta().await.unwrap();
    assert_eq!(meta.runs, 1);
    let patched = c
        .patch_run(
            RUN,
            &serde_json::from_value(json!({"name": "renamed"})).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(patched.run.name, "renamed");

    // Errors carry the status, the code and the engine's message.
    let missing = api(c.run("nope").await.unwrap_err());
    assert_eq!((missing.status, missing.code.as_str()), (404, "not_found"));
    let bad = api(c
        .runs(&RunsQuery {
            q: "lr<".into(),
            ..Default::default()
        })
        .await
        .unwrap_err());
    assert_eq!((bad.status, bad.code.as_str()), (400, "invalid_argument"));
    assert!(bad.position.is_some());
    let anonymous = Client::new(&url, None);
    assert!(anonymous.info().await.is_ok());
    let denied = api(anonymous.meta().await.unwrap_err());
    assert_eq!((denied.status, denied.code.as_str()), (401, "unauthorized"));
    let unreachable = Client::new("http://127.0.0.1:9", None).info().await;
    assert!(matches!(unreachable, Err(Error::Unreachable { .. })));

    // Tokens: a read+ingest token works, and stops working once revoked.
    let created = c
        .create_token(
            &serde_json::from_value(json!({"name": "ci", "scopes": ["read", "ingest"]})).unwrap(),
        )
        .await
        .unwrap();
    assert!(c
        .tokens()
        .await
        .unwrap()
        .tokens
        .iter()
        .any(|t| t.id == created.info.id));
    let ci = Client::new(&url, Some(created.token.clone()));
    assert_eq!(ci.runs(&RunsQuery::default()).await.unwrap().total, 1);
    assert_eq!(api(ci.optimize().await.unwrap_err()).code, "forbidden");
    assert!(c.revoke_token(&created.info.id).await.unwrap().revoked);
    assert_eq!(api(ci.meta().await.unwrap_err()).status, 401);

    // Datasets.
    let v = c
        .commit_dataset(
            "questions",
            &serde_json::from_value(
                json!({"items": [{"id": "q1", "input": "2+2", "expected": "4"}]}),
            )
            .unwrap(),
            "commit-1",
        )
        .await
        .unwrap();
    assert_eq!(v.version, 1);
    assert_eq!(c.datasets().await.unwrap().len(), 1);
    assert_eq!(c.dataset_versions("questions").await.unwrap().len(), 1);
    assert_eq!(c.dataset_items("questions", 1).await.unwrap().len(), 1);

    // Traces.
    let spans = serde_json::from_value(json!({"spans": [{
        "trace_id": "4bf92f3577b34da6a3ce929d0e0e4736", "span_id": "00f067aa0ba902b7",
        "name": "answer", "start_time": 1_700_000_000_000_000i64,
        "end_time": 1_700_000_000_500_000i64
    }]}))
    .unwrap();
    c.ingest_spans(&spans).await.unwrap();
    let traces = c.traces(&Default::default()).await.unwrap();
    assert_eq!(traces.rows.len(), 1);
    assert!(
        c.delete_trace("4bf92f3577b34da6a3ce929d0e0e4736")
            .await
            .unwrap()
            .deleted
    );

    // Admin.
    let finished = c
        .finish_run(
            RUN,
            &serde_json::from_value(json!({"status": "finished"})).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(finished.status, vigily_api::model::RunStatus::Finished);
    assert_eq!(
        c.delete_runs(&serde_json::from_value(json!({"ids": [RUN]})).unwrap())
            .await
            .unwrap()
            .deleted,
        1
    );
    c.optimize().await.unwrap();
    assert_eq!(c.purge(false).await.unwrap().runs, 1);
}

#[tokio::test]
async fn following_a_schedule_run_waits_for_its_retries() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config {
        schedule: vec![
            schedule(dir.path(), "ok", &["sh", "-c", "echo done"], 0),
            schedule(dir.path(), "flaky", &["sh", "-c", "exit 3"], 1),
        ],
        ..Default::default()
    };
    let (url, token) = engine(dir.path(), config).await;
    let c = Client::new(&url, Some(token));
    assert_eq!(c.schedules().await.unwrap().schedules.len(), 2);

    let started = c.start_schedule_run("ok").await.unwrap();
    let done = c
        .follow_schedule_run("ok", &started.id, |_| {})
        .await
        .unwrap();
    assert_eq!(
        done.run.status,
        ScheduleRunStatus::Succeeded,
        "{:?} {}",
        done.run.reason,
        done.run.stderr
    );
    assert_eq!(done.run.stdout.trim(), "done");

    let started = c.start_schedule_run("flaky").await.unwrap();
    let mut seen = Vec::new();
    let last = c
        .follow_schedule_run("flaky", &started.id, |a| {
            seen.push((a.run.attempt, a.run.status))
        })
        .await
        .unwrap();
    assert_eq!(
        seen,
        vec![
            (1, ScheduleRunStatus::Failed),
            (2, ScheduleRunStatus::Failed)
        ]
    );
    assert_eq!(last.run.exit_code, Some(3));
    assert_eq!(c.schedule_runs("flaky").await.unwrap().runs.len(), 2);

    let missing = api(c.start_schedule_run("nope").await.unwrap_err());
    assert_eq!(missing.code, "not_found");
}

/// Stops the engine an engine file names.
fn stop(pid: u32) {
    #[cfg(unix)]
    // SAFETY: kill only sends a signal to an engine this test started.
    unsafe {
        libc::kill(pid as i32, libc::SIGTERM);
    }
}

fn resolve_in(state: &Path, repo: &Path) -> Resolve {
    Resolve {
        repo: repo.to_str().unwrap().to_string(),
        state_dir: Some(state.to_path_buf()),
        engine: Some(ENGINE.into()),
        port: Some(0),
        ..Default::default()
    }
}

#[tokio::test]
async fn resolve_uses_the_given_address_without_asking() {
    let r = Resolve {
        repo: "anything".into(),
        url: Some("http://127.0.0.1:9/".into()),
        token: Some("t".into()),
        ..Default::default()
    };
    let (c, found) = resolve(&r).await.unwrap();
    assert_eq!(found, Found::Url);
    assert_eq!((c.base(), c.token()), ("http://127.0.0.1:9", Some("t")));
}

#[tokio::test]
async fn resolve_starts_one_engine_for_racing_clients_and_then_finds_it() {
    let dir = tempfile::tempdir().unwrap();
    let (state, repo) = (dir.path().join("state"), dir.path().join("repo"));
    let mut r = resolve_in(&state, &repo);

    r.start = false;
    assert!(matches!(
        resolve(&r).await,
        Err(ResolveError::NotRunning { .. })
    ));

    r.start = true;
    let (a, b, c) = tokio::join!(resolve(&r), resolve(&r), resolve(&r));
    let (a, b, c) = (a.unwrap(), b.unwrap(), c.unwrap());
    assert_eq!(a.0.base(), b.0.base());
    assert_eq!(a.0.base(), c.0.base());
    assert_ne!(a.0.base(), "http://127.0.0.1:7600");
    let pid = match &a.1 {
        Found::Started(f) | Found::Running(f) => f.pid,
        Found::Url => unreachable!(),
    };
    for (_, found) in [&a, &b, &c] {
        match found {
            Found::Started(f) | Found::Running(f) => {
                assert_eq!(f.pid, pid);
                assert!(f.auto_started);
            }
            Found::Url => unreachable!(),
        }
    }
    // The engine file's token opens the API.
    assert_eq!(a.0.runs(&RunsQuery::default()).await.unwrap().total, 0);

    r.start = false;
    let (again, found) = resolve(&r).await.unwrap();
    assert!(matches!(found, Found::Running(_)));
    assert_eq!(again.base(), a.0.base());
    stop(pid);
}

#[tokio::test]
async fn resolve_ignores_an_engine_file_left_by_a_crash() {
    let dir = tempfile::tempdir().unwrap();
    let (state, repo) = (dir.path().join("state"), dir.path().join("repo"));
    let paths = vigily_api::local::EnginePaths::new(
        &state,
        &vigily_api::local::canonical_location(repo.to_str().unwrap()),
    );
    std::fs::create_dir_all(&paths.dir).unwrap();
    std::fs::write(
        &paths.json,
        json!({"url": "http://127.0.0.1:9", "pid": 1, "version": "0", "api_version": "v1",
               "repo": "x", "mode": "local", "started_at": 0})
        .to_string(),
    )
    .unwrap();
    let r = Resolve {
        start: false,
        ..resolve_in(&state, &repo)
    };
    assert!(matches!(
        resolve(&r).await,
        Err(ResolveError::NotRunning { .. })
    ));
    let (c, found) = resolve(&Resolve { start: true, ..r }).await.unwrap();
    assert!(matches!(found, Found::Started(_)));
    assert_ne!(c.base(), "http://127.0.0.1:9");
    if let Found::Started(f) = found {
        stop(f.pid);
    }
}
