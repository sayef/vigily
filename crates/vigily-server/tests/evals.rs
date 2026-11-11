use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;
use vigily_core::evals::{
    self, CommitMode, EvalResult, FeatureSample, ItemInput, ScheduleRunStatus, Score,
};
use vigily_core::{MemoryRecords, MemoryStore, NewRun, RunStatus, Store, TrackOpts};
use vigily_server::config::{Config, ScheduleConfig};
use vigily_server::{monitor, router, scheduler, AppState};

async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value, Option<String>) {
    let mut req = Request::builder().method(method).uri(uri);
    let body = match body {
        Some(b) => {
            req = req.header("content-type", "application/json");
            Body::from(b.to_string())
        }
        None => Body::empty(),
    };
    let res = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let status = res.status();
    let disp = res
        .headers()
        .get("content-disposition")
        .map(|v| v.to_str().unwrap().to_string());
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        disp,
    )
}

/// Ten nightly eval runs of one target; the last one drops from 90 % to 60 % and sees longer inputs.
async fn setup() -> (Arc<AppState>, axum::Router, Vec<String>) {
    let store = MemoryStore::new();
    let records = MemoryRecords::new();
    let items: Vec<ItemInput> = (0..200)
        .map(|i| ItemInput {
            id: Some(format!("q{i:03}")),
            input: json!(format!("question {i}")),
            expected: json!("a"),
            metadata: json!({}),
        })
        .collect();
    let v = evals::commit_dataset(
        &records,
        "support",
        items,
        CommitMode::Replace,
        None,
        Some("Support questions".into()),
        Some("sdk".into()),
    )
    .await
    .unwrap();
    let mut ids = Vec::new();
    for night in 0..10 {
        let w = store
            .create_run(NewRun {
                name: Some(format!("nightly-{night}")),
                experiment: Some("support-nightly".into()),
                kind: Some("eval".into()),
                target: Some(json!({"model": "clf-v2"})),
                created_at: Some(1_000_000_000 * night),
                ..Default::default()
            })
            .await
            .unwrap();
        w.set_params(&json!({"__eval__": {"dataset": {"name": "support", "version": 1, "digest": v.digest, "items": 200},
            "evaluators": [{"name": "exact_match", "version": "vigily/exact_match@1", "kind": "exact_match"}]},
            "__run__": {"schedule": {"name": "nightly", "run": "00000000000000000001-1"}}}))
            .unwrap();
        let run_id = w.info().id;
        let fail_from = if night == 9 { 120 } else { 180 };
        let results: Vec<EvalResult> = v
            .items
            .iter()
            .enumerate()
            .map(|(i, r)| EvalResult {
                run_id: run_id.clone(),
                item_id: r.id.clone(),
                item_hash: r.hash.clone(),
                dataset: "support".into(),
                dataset_version: 1,
                output: json!(if i < fail_from { "a" } else { "b" }),
                error: None,
                latency_ms: Some(10.0),
                scores: vec![Score {
                    name: "exact_match".into(),
                    value: Some(if i < fail_from { 1.0 } else { 0.0 }),
                    passed: Some(i < fail_from),
                    reason: None,
                    evaluator: "exact_match".into(),
                    evaluator_version: "vigily/exact_match@1".into(),
                }],
                created_at: 0,
                trace_id: None,
            })
            .collect();
        evals::put_results(&records, results).await.unwrap();
        let length = |shift: f64| {
            (0..300)
                .map(|i| json!((i % 50) as f64 + shift))
                .collect::<Vec<_>>()
        };
        evals::put_features(
            &records,
            vec![FeatureSample {
                run_id: run_id.clone(),
                feature: "length".into(),
                values: length(if night == 9 { 40.0 } else { 0.0 }),
            }],
        )
        .await
        .unwrap();
        w.track(
            "eval/exact_match",
            fail_from as f64 / 200.0,
            TrackOpts {
                step: Some(200),
                ..Default::default()
            },
        )
        .unwrap();
        w.finish(
            RunStatus::Finished,
            Some(1_000_000_000 * night + 60_000_000),
        )
        .await
        .unwrap();
        ids.push(run_id);
    }
    let state = AppState::with(Arc::new(store), Arc::new(records), Config::default());
    state.catalog.refresh().await.unwrap();
    (state.clone(), router(state), ids)
}

#[tokio::test]
async fn datasets_items_labels_compare_and_audit() {
    let (_, app, ids) = setup().await;
    let (st, v, _) = call(&app, "GET", "/api/datasets", None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v[0]["name"], "support");
    let (_, v, _) = call(
        &app,
        "GET",
        "/api/datasets/support?q=question%2019&limit=5",
        None,
    )
    .await;
    assert_eq!(v["version"]["version"], 1);
    assert_eq!(v["total"], 11, "question 19 and question 190-199");
    assert_eq!(v["runs"].as_array().unwrap().len(), 10);

    let last = &ids[9];
    let (_, v, _) = call(
        &app,
        "GET",
        &format!("/api/runs/{last}/items?failures=true"),
        None,
    )
    .await;
    assert_eq!(
        (v["total"].as_u64(), v["failed"].as_u64()),
        (Some(200), Some(80))
    );
    assert_eq!(v["items"].as_array().unwrap().len(), 80);
    assert_eq!(v["items"][0]["input"], "question 120");
    assert_eq!(v["scores"]["exact_match"]["pass_rate"], 0.6);

    let (st, l, _) = call(&app, "POST", &format!("/api/runs/{last}/labels"), Some(json!({"item_id": "q150", "name": "human", "passed": true, "comment": "b is acceptable"}))).await;
    assert_eq!(st, StatusCode::OK, "{l}");
    let (st, _, _) = call(
        &app,
        "POST",
        &format!("/api/runs/{last}/labels"),
        Some(json!({"item_id": "q150", "name": " "})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _, _) = call(
        &app,
        "POST",
        &format!("/api/runs/{last}/labels"),
        Some(json!({"item_id": "nope", "name": "human", "passed": true})),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    let (_, c, _) = call(
        &app,
        "GET",
        &format!("/api/compare?a={}&b={last}", ids[8]),
        None,
    )
    .await;
    assert_eq!(c["counts"]["regressed"], 60);
    assert_eq!(c["items"][0]["change"], "regressed");

    let (st, a, disp) = call(
        &app,
        "GET",
        &format!("/api/runs/{last}/audit?download=true"),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert!(disp.unwrap().contains("attachment"));
    assert_eq!(a["format"], "vigily.audit/1");
    assert_eq!(a["dataset"]["version"], 1);
    assert_eq!(a["target"]["model"], "clf-v2");
    assert_eq!(a["schedule"]["name"], "nightly");
    assert_eq!(a["results"].as_array().unwrap().len(), 200);
    assert_eq!(
        a["results"][0]["scores"][0]["evaluator_version"],
        "vigily/exact_match@1"
    );
    assert_eq!(a["labels"][0]["comment"], "b is acceptable");
}

#[tokio::test]
async fn suites_drift_baselines_and_alerts() {
    let (state, app, ids) = setup().await;
    let (_, v, _) = call(&app, "GET", "/api/suites", None).await;
    assert_eq!(v.as_array().unwrap().len(), 1);
    let s = &v[0];
    assert_eq!(s["label"], "clf-v2");
    assert_eq!(s["behaviour"][0]["drifted"], true);
    assert_eq!(s["data"], "drift");
    let key = s["key"].as_str().unwrap().to_string();

    let (_, d, _) = call(&app, "GET", &format!("/api/suites/{key}"), None).await;
    let hist = d["history"]["exact_match"].as_array().unwrap();
    assert_eq!(hist.len(), 10);
    assert_eq!(hist[9]["drifted"], true);
    assert_eq!(hist[8]["drifted"], false);
    assert_eq!(d["drift"][0]["regressions"].as_array().unwrap().len(), 60);
    assert_eq!(d["drift"][0]["regression_inputs"][0], "question 120");

    let (_, dd, _) = call(&app, "GET", &format!("/api/suites/{key}/data-drift"), None).await;
    assert_eq!(dd["features"][0]["level"], "drift");
    assert_eq!(dd["baseline_runs"].as_array().unwrap().len(), 7);

    let (st, _, _) = call(
        &app,
        "PUT",
        &format!("/api/suites/{key}/baseline"),
        Some(json!({"mode": "pinned", "window": 7, "runs": ["nope"], "updated_at": 0})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, b, _) = call(
        &app,
        "PUT",
        &format!("/api/suites/{key}/baseline"),
        Some(json!({"mode": "pinned", "window": 7, "runs": [ids[0], ids[1]], "updated_at": 0})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{b}");
    let (_, dd, _) = call(&app, "GET", &format!("/api/suites/{key}/data-drift"), None).await;
    assert_eq!(dd["baseline_runs"].as_array().unwrap().len(), 2);

    let checked = monitor::Checked::default();
    assert_eq!(
        monitor::check(&state, &checked).await.unwrap(),
        2,
        "behaviour and data drift"
    );
    assert_eq!(
        monitor::check(&state, &checked).await.unwrap(),
        0,
        "checked once"
    );
    let (_, al, _) = call(&app, "GET", "/api/alerts", None).await;
    assert_eq!(al.as_array().unwrap().len(), 2);
    let id = al[0]["id"].as_str().unwrap();
    let (_, a, _) = call(
        &app,
        "POST",
        &format!("/api/alerts/{id}"),
        Some(json!({"acknowledged": true})),
    )
    .await;
    assert!(a["acknowledged_at"].is_number());
    let (_, m, _) = call(&app, "GET", "/api/meta", None).await;
    assert_eq!(m["open_alerts"], 1);
    assert_eq!(m["datasets"], 1);
}

#[cfg(unix)]
#[tokio::test]
async fn schedules_run_retry_capture_output_and_time_out() {
    let dir = tempfile::tempdir().unwrap();
    let mk = |name: &str, cmd: &[&str], timeout: &str, retries: u32| ScheduleConfig {
        name: name.into(),
        description: None,
        cron: "0 2 * * *".into(),
        timezone: "Europe/Berlin".into(),
        command: cmd.iter().map(|s| s.to_string()).collect(),
        workdir: None,
        env: [("GREETING".to_string(), "hello".to_string())].into(),
        timeout: timeout.into(),
        retries,
        retry_delay: "0s".into(),
        missed: Default::default(),
        enabled: true,
        keep_runs: 200,
        keep_days: 90,
        keep_output: 50,
    };
    let config = Config {
        schedule: vec![
            mk(
                "flaky",
                &[
                    "sh",
                    "-c",
                    "echo $GREETING $VIGILY_SCHEDULE; echo oops >&2; exit 3",
                ],
                "10s",
                1,
            ),
            mk("slow", &["sleep", "5"], "1s", 0),
            mk("ok", &["true"], "10s", 0),
        ],
        base_dir: dir.path().to_path_buf(),
        ..Default::default()
    };
    let state = AppState::with(
        Arc::new(MemoryStore::new()),
        Arc::new(MemoryRecords::new()),
        config,
    );
    let runs = scheduler::execute(&state, &state.config.schedule[0], 42).await;
    assert_eq!(runs.len(), 2);
    assert!(runs
        .iter()
        .all(|r| r.status == ScheduleRunStatus::Failed && r.exit_code == Some(3)));
    assert_eq!(runs[0].stdout.trim(), "hello flaky");
    assert_eq!(runs[0].stderr.trim(), "oops");
    let started = std::time::Instant::now();
    let slow = scheduler::execute(&state, &state.config.schedule[1], 42).await;
    assert_eq!(slow[0].status, ScheduleRunStatus::TimedOut);
    assert!(started.elapsed() < std::time::Duration::from_secs(4));
    let ok = scheduler::execute(&state, &state.config.schedule[2], 42).await;
    assert_eq!(ok[0].status, ScheduleRunStatus::Succeeded);

    let app = router(state.clone());
    let (_, v, _) = call(&app, "GET", "/api/schedules", None).await;
    let list = v["schedules"].as_array().unwrap();
    assert_eq!(list.len(), 3);
    assert_eq!(list[0]["last_run"]["status"], "failed");
    assert_eq!(list[0]["runs"], 2);
    let (_, r, _) = call(&app, "GET", "/api/schedules/flaky/runs", None).await;
    let id = r["runs"][0]["id"].as_str().unwrap();
    let (_, full, _) = call(
        &app,
        "GET",
        &format!("/api/schedules/flaky/runs/{id}"),
        None,
    )
    .await;
    assert_eq!(full["stderr"].as_str().unwrap().trim(), "oops");
    let (_, al, _) = call(&app, "GET", "/api/alerts", None).await;
    let kinds: Vec<&str> = al
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        vec!["schedule_failed", "schedule_failed"],
        "flaky and slow"
    );
    let (st, _, _) = call(&app, "GET", "/api/schedules/nope/runs", None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

/// True while a process with this id exists (zombies of other parents count as gone soon).
#[cfg(unix)]
fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid, 0) == 0 }
}

#[cfg(unix)]
async fn gone_within(pid: i32, secs: u64) -> bool {
    for _ in 0..secs * 10 {
        if !alive(pid) {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    false
}

/// A command that exits but leaves a process holding its output: the attempt still completes
/// promptly, and the leftover process is stopped. A process that started its own session is
/// stopped too (on Linux, by the run's environment marker).
#[cfg(unix)]
#[tokio::test]
async fn background_processes_do_not_block_or_survive() {
    let dir = tempfile::tempdir().unwrap();
    let mk = |cmd: &str, timeout: &str| ScheduleConfig {
        name: "s".into(),
        description: None,
        cron: "0 2 * * *".into(),
        timezone: "UTC".into(),
        command: vec!["sh".into(), "-c".into(), cmd.into()],
        workdir: None,
        env: Default::default(),
        timeout: timeout.into(),
        retries: 0,
        retry_delay: "0s".into(),
        missed: Default::default(),
        enabled: true,
        keep_runs: 200,
        keep_days: 90,
        keep_output: 50,
    };
    let mut cases = vec![("sleep 30 & echo $!", "10s", ScheduleRunStatus::Succeeded)];
    if cfg!(target_os = "linux") {
        cases.push((
            "setsid sleep 30 & echo $!; sleep 30",
            "2s",
            ScheduleRunStatus::TimedOut,
        ));
    }
    for (cmd, timeout, want) in cases {
        let config = Config {
            schedule: vec![mk(cmd, timeout)],
            base_dir: dir.path().to_path_buf(),
            ..Default::default()
        };
        let state = AppState::with(
            Arc::new(MemoryStore::new()),
            Arc::new(MemoryRecords::new()),
            config,
        );
        let t = std::time::Instant::now();
        let runs = scheduler::execute(&state, &state.config.schedule[0], 42).await;
        let took = t.elapsed().as_secs_f64();
        assert_eq!(runs[0].status, want, "{cmd}");
        assert!(took < 9.0, "{cmd}: took {took:.1} s");
        let pid: i32 = runs[0]
            .stdout
            .trim()
            .parse()
            .unwrap_or_else(|_| panic!("{cmd}: stdout {:?}", runs[0].stdout));
        assert!(
            gone_within(pid, 5).await,
            "{cmd}: background process {pid} survived"
        );
    }
}

/// A finished eval run whose results arrive later is checked again when they do.
#[tokio::test]
async fn monitor_rechecks_runs_whose_results_arrive_late() {
    let (state, _app, ids) = setup().await;
    let last = ids[9].clone();
    // Hide the newest run's results, as if they were still buffered in the evaluating process.
    let saved = evals::results(&*state.records, &last).await.unwrap();
    state
        .records
        .delete(
            saved
                .iter()
                .map(|r| format!("res/{last}/{}", r.item_id))
                .collect(),
        )
        .await
        .unwrap();
    let checked = monitor::Checked::default();
    let first = monitor::check(&state, &checked).await.unwrap();
    assert_eq!(first, 1, "only the data drift is visible without results");
    evals::put_results(&*state.records, saved).await.unwrap();
    assert_eq!(
        monitor::check(&state, &checked).await.unwrap(),
        1,
        "the behaviour drift once results arrive"
    );
    assert_eq!(monitor::check(&state, &checked).await.unwrap(), 0);
}

/// Delivery to the webhook, and an acknowledgement made while delivery is slow survives it.
#[tokio::test]
async fn webhook_delivery_keeps_acknowledgements() {
    use axum::routing::post;
    let got = Arc::new(parking_lot::Mutex::new(Vec::<Value>::new()));
    let sink = got.clone();
    let hook = axum::Router::new().route(
        "/hook",
        post(move |axum::Json(v): axum::Json<Value>| {
            let sink = sink.clone();
            async move {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                sink.lock().push(v);
                "ok"
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, hook).await.unwrap() });
    let mut config = Config::default();
    config.alerts.webhook = Some(format!("http://{addr}/hook"));
    let state = AppState::with(
        Arc::new(MemoryStore::new()),
        Arc::new(MemoryRecords::new()),
        config,
    );
    let alert = evals::Alert {
        id: String::new(),
        kind: evals::AlertKind::ScheduleFailed,
        severity: evals::Severity::Critical,
        title: "Schedule nightly failed".into(),
        detail: "exit 2".into(),
        suite: None,
        run_id: None,
        schedule: Some("nightly".into()),
        created_at: 0,
        acknowledged_at: None,
        dedupe: "schedule/nightly/1".into(),
        delivered: None,
    };
    let st = state.clone();
    let task = tokio::spawn(async move { monitor::notify(&st, alert).await });
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    let mut a = evals::alerts(&*state.records).await.unwrap().remove(0);
    a.acknowledged_at = Some(7);
    evals::update_alert(&*state.records, &a).await.unwrap();
    assert!(task.await.unwrap(), "a new alert");
    // Delivery runs on its own task; wait for its outcome.
    let mut a = evals::alerts(&*state.records).await.unwrap().remove(0);
    for _ in 0..50 {
        if a.delivered.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        a = evals::alerts(&*state.records).await.unwrap().remove(0);
    }
    assert_eq!((a.acknowledged_at, a.delivered), (Some(7), Some(true)));
    let body = got.lock();
    assert_eq!(body[0]["event"], "vigily.alert");
    assert_eq!(body[0]["alert"]["title"], "Schedule nightly failed");
}
