//! The routes added with `/api/v1`: paging the runs list, a run's parameters, series and points,
//! dataset versions and items, eval results, storage administration, deleting traces, starting a
//! schedule and the tracing policy.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;
use vigily_core::evals::{self, CommitMode, EvalResult, ItemInput, Score};
use vigily_core::traces::{PayloadMode, ProjectTracing};
use vigily_core::{
    Error as CoreError, MemoryRecords, MemoryStore, NewRun, Points, PurgeSummary, RecordStore,
    Result as CoreResult, Run, RunPatch, RunStatus, RunWriter, Series, StepRange, Store, TrackOpts,
};
use vigily_server::config::{Config, ScheduleConfig};
use vigily_server::{router, AppState};

struct App {
    app: axum::Router,
    runs: Vec<String>,
    records: Arc<MemoryRecords>,
}

async fn setup(store: Arc<dyn Store>, config: Config) -> App {
    let mut runs = Vec::new();
    for i in 0..5 {
        let w = store
            .create_run(NewRun {
                name: Some(format!("run-{i}")),
                experiment: Some("sweep".into()),
                created_at: Some(1_000_000 * (i + 1)),
                ..Default::default()
            })
            .await
            .unwrap();
        w.set_params(&json!({"hparams": {"lr": 0.1 / (i + 1) as f64}}))
            .unwrap();
        for s in 0..10 {
            let v = if s == 3 && i == 0 {
                f64::NAN
            } else {
                1.0 / (s as f64 + 1.0)
            };
            for subset in ["train", "val"] {
                w.track(
                    "loss",
                    v,
                    TrackOpts {
                        step: Some(s),
                        context: Some(json!({ "subset": subset })),
                        timestamp: Some(10_000_000 + s * 1_000_000),
                        ..Default::default()
                    },
                )
                .unwrap();
            }
        }
        w.finish(RunStatus::Finished, None).await.unwrap();
        runs.push(w.info().id);
    }
    let records = Arc::new(MemoryRecords::new());
    let state = AppState::with(store, records.clone(), config);
    state.catalog.refresh().await.unwrap();
    App {
        app: router(state),
        runs,
        records,
    }
}

async fn app() -> App {
    setup(Arc::new(MemoryStore::new()), Config::default()).await
}

async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, HeaderMap, Value) {
    let res = app.clone().oneshot(req).await.unwrap();
    let (status, headers) = (res.status(), res.headers().clone());
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let v = serde_json::from_slice(&bytes)
        .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, headers, v)
}

async fn get(app: &axum::Router, uri: &str) -> (StatusCode, Value) {
    let req = Request::get(uri)
        .header("host", "localhost")
        .body(Body::empty())
        .unwrap();
    let (st, _, v) = send(app, req).await;
    (st, v)
}

async fn post(app: &axum::Router, uri: &str, body: Value) -> (StatusCode, HeaderMap, Value) {
    let req = Request::post(uri)
        .header("host", "localhost")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    send(app, req).await
}

#[tokio::test]
async fn runs_page_with_limit_offset_and_fields() {
    let a = app().await;
    let (st, v) = get(&a.app, "/api/v1/runs?sort=run:name").await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        (v["total"].as_u64(), v["matched"].as_u64()),
        (Some(5), Some(5))
    );
    assert_eq!(v["rows"].as_array().unwrap().len(), 5);
    assert!(v["rows"][0]["params"].is_object() && v["rows"][0]["metrics"].is_object());

    let (_, v) = get(&a.app, "/api/v1/runs?sort=run:name&limit=2&offset=1").await;
    let names: Vec<&str> = v["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["run-1", "run-2"]);
    assert_eq!(v["matched"], 5);

    let (_, v) = get(&a.app, "/api/v1/runs?q=lr%3C0.03&fields=summary").await;
    assert_eq!(
        (v["total"].as_u64(), v["matched"].as_u64()),
        (Some(5), Some(2))
    );
    let row = &v["rows"][0];
    assert!(
        row.get("params").is_none() && row.get("metrics").is_none(),
        "{row}"
    );
    assert_eq!(row["experiment"], "sweep");

    let (_, v) = get(&a.app, "/api/v1/runs?offset=10").await;
    assert_eq!(v["rows"].as_array().unwrap().len(), 0);
    for bad in ["limit=0", "limit=10001", "fields=everything"] {
        let (st, v) = get(&a.app, &format!("/api/v1/runs?{bad}")).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{bad}");
        assert_eq!(v["error"]["code"], "invalid_argument", "{bad}: {v}");
    }
}

#[tokio::test]
async fn a_runs_params_series_and_points() {
    let a = app().await;
    let id = &a.runs[0];
    let (st, v) = get(&a.app, &format!("/api/v1/runs/{id}/params")).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["run_id"], id.as_str());
    assert_eq!(v["params"]["hparams"]["lr"], 0.1);

    let (_, v) = get(&a.app, &format!("/api/v1/runs/{id}/series")).await;
    let keys: Vec<&str> = v["series"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys.len(), 2);
    assert!(keys.contains(&"loss[val]"), "{keys:?}");
    assert_eq!(v["series"][0]["summary"]["count"], 10);

    let ctx = "%7B%22subset%22%3A%22val%22%7D";
    let (st, v) = get(
        &a.app,
        &format!("/api/v1/runs/{id}/points?name=loss&context={ctx}"),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["key"], "loss[val]");
    assert_eq!(v["steps"].as_array().unwrap().len(), 10);
    assert_eq!(v["values"][0], 1.0);
    assert_eq!(v["values"][3], "NaN", "non-finite values are strings");
    let back: vigily_api::runs::SeriesPoints = serde_json::from_value(v.clone()).unwrap();
    assert!(back.values[3].is_nan());

    // A time window: [12 s, 15 s) holds steps 2, 3 and 4.
    let (_, v) = get(
        &a.app,
        &format!(
            "/api/v1/runs/{id}/points?name=loss&context={ctx}&start_time=12000000&stop_time=15000000"
        ),
    )
    .await;
    assert_eq!(v["steps"], json!([2, 3, 4]));

    let (st, v) = get(&a.app, &format!("/api/v1/runs/{id}/points?name=loss")).await;
    assert_eq!(st, StatusCode::NOT_FOUND, "no series without context: {v}");
    assert_eq!(v["error"]["code"], "not_found");
    let (st, v) = get(
        &a.app,
        &format!("/api/v1/runs/{id}/points?name=loss&context=val"),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    for path in ["params", "series", "points?name=loss"] {
        let (st, _) = get(&a.app, &format!("/api/v1/runs/nope/{path}")).await;
        assert_eq!(st, StatusCode::NOT_FOUND, "{path}");
    }
}

#[tokio::test]
async fn dataset_versions_items_and_results() {
    let a = app().await;
    let items = |n: usize| {
        (0..n)
            .map(|i| ItemInput {
                id: Some(format!("q{i}")),
                input: json!({"question": format!("question {i}")}),
                expected: json!(i),
                metadata: Value::Null,
            })
            .collect::<Vec<_>>()
    };
    let rs: &dyn RecordStore = &*a.records;
    evals::commit_dataset(rs, "qa", items(3), CommitMode::Replace, None, None, None)
        .await
        .unwrap();
    evals::commit_dataset(rs, "qa", items(600), CommitMode::Replace, None, None, None)
        .await
        .unwrap();

    let (st, v) = get(&a.app, "/api/v1/datasets/qa/versions").await;
    assert_eq!(st, StatusCode::OK);
    let versions: Vec<u64> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["version"].as_u64().unwrap())
        .collect();
    assert_eq!(versions, [2, 1], "newest first");
    assert_eq!(v[0]["items"], 600);

    let (st, v) = get(&a.app, "/api/v1/datasets/qa?version=1").await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        (v["version"]["version"].as_u64(), v["total"].as_u64()),
        (Some(1), Some(3))
    );

    let req = Request::get("/api/v1/datasets/qa/versions/2/items")
        .header("host", "localhost")
        .body(Body::empty())
        .unwrap();
    let res = a.app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers().get("content-type").unwrap(),
        "application/x-ndjson"
    );
    let text =
        String::from_utf8(res.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
    let lines: Vec<Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 600);
    assert_eq!(lines[0]["id"], "q0");
    assert_eq!(lines[599]["expected"], 599);
    assert!(text.ends_with('\n'));

    for (uri, why) in [
        ("/api/v1/datasets/nope/versions", "no dataset"),
        ("/api/v1/datasets/nope/versions/1/items", "no dataset"),
        ("/api/v1/datasets/qa/versions/9/items", "no version"),
    ] {
        let (st, v) = get(&a.app, uri).await;
        assert_eq!(st, StatusCode::NOT_FOUND, "{why}: {v}");
    }

    let run = &a.runs[1];
    let result = EvalResult {
        run_id: run.clone(),
        item_id: "q0".into(),
        item_hash: "h".into(),
        dataset: "qa".into(),
        dataset_version: 2,
        output: json!(0),
        error: None,
        latency_ms: Some(12.5),
        scores: vec![Score {
            name: "exact_match".into(),
            value: Some(1.0),
            passed: Some(true),
            reason: None,
            evaluator: "exact_match".into(),
            evaluator_version: "1".into(),
        }],
        created_at: 1,
        trace_id: None,
    };
    evals::put_results(rs, vec![result.clone()]).await.unwrap();
    let (st, v) = get(&a.app, &format!("/api/v1/runs/{run}/results")).await;
    assert_eq!(st, StatusCode::OK);
    let got: Vec<EvalResult> = serde_json::from_value(v).unwrap();
    assert_eq!(got, vec![result]);
    let (st, _) = get(&a.app, "/api/v1/runs/nope/results").await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

/// A store whose purge is refused because a tag keeps deleted data, as kv0 refuses it.
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
async fn purge_and_optimize() {
    let a = app().await;
    let (st, _, v) = post(&a.app, "/api/v1/admin/purge", json!({})).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["runs"], 0);
    assert_eq!(v["legacy_records_present"], false);
    let summary: vigily_api::runs::PurgeSummary = serde_json::from_value(v).unwrap();
    assert!(summary.tags_deleted.is_empty());

    let (st, _, v) = post(&a.app, "/api/v1/admin/optimize", json!({})).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert!(v["seconds"].as_f64().unwrap() >= 0.0);

    let t = setup(Arc::new(Tagged(MemoryStore::new())), Config::default()).await;
    let (st, _, v) = post(&t.app, "/api/v1/admin/purge", json!({"force": false})).await;
    assert_eq!(st, StatusCode::CONFLICT);
    assert_eq!(v["error"]["code"], "tag_blocks_purge");
    let msg = v["error"]["message"].as_str().unwrap();
    assert!(
        msg.starts_with("storage error: tag before-cleanup"),
        "{msg}"
    );
    assert!(msg.contains("Run `vigily runs purge --force`"), "{msg}");
    let (st, _, v) = post(&t.app, "/api/v1/admin/purge", json!({"force": true})).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["tags_deleted"], json!(["before-cleanup"]));
}

#[tokio::test]
async fn admin_writes_refuse_web_pages() {
    let a = app().await;
    for uri in [
        "/api/v1/admin/purge",
        "/api/v1/admin/optimize",
        "/api/v1/traces/delete",
    ] {
        let req = Request::post(uri)
            .header("host", "localhost")
            .header("content-type", "text/plain")
            .body(Body::from("{}"))
            .unwrap();
        let (st, _, v) = send(&a.app, req).await;
        assert_eq!(st, StatusCode::UNSUPPORTED_MEDIA_TYPE, "{uri}: {v}");
        assert_eq!(v["error"]["code"], "invalid_argument", "{uri}");

        let req = Request::post(uri)
            .header("host", "localhost:7600")
            .header("origin", "http://evil.example")
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();
        let (st, _, v) = send(&a.app, req).await;
        assert_eq!(st, StatusCode::FORBIDDEN, "{uri}: {v}");
        assert_eq!(v["error"]["code"], "forbidden", "{uri}");
    }
    // The web app's own origin is fine.
    let req = Request::post("/api/v1/admin/optimize")
        .header("host", "localhost:7600")
        .header("origin", "http://localhost:7600")
        .header("content-type", "application/json")
        .body(Body::empty())
        .unwrap();
    let (st, _, v) = send(&a.app, req).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    // New routes have no deprecated alias.
    let (st, _, _) = post(&a.app, "/api/admin/optimize", json!({})).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn traces_delete_in_bulk() {
    let a = app().await;
    let span = |trace: &str| {
        json!({"trace_id": trace, "span_id": format!("{trace}-root"), "name": "answer",
               "start_time": 1_000_000, "end_time": 2_000_000})
    };
    let (st, _, v) = post(
        &a.app,
        "/api/v1/traces/ingest",
        json!({"spans": [span("t1"), span("t2"), span("t3")]}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");

    let (st, _, v) = post(
        &a.app,
        "/api/v1/traces/delete",
        json!({"ids": ["t1", "t2", "nope"]}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v, json!({"deleted": 2, "missing": ["nope"], "purge": null}));
    let (_, v) = get(&a.app, "/api/v1/traces").await;
    assert_eq!(v["total"], 1);
    assert_eq!(v["rows"][0]["id"], "t3");

    let (st, _, v) = post(
        &a.app,
        "/api/v1/traces/delete",
        json!({"ids": ["t3"], "erase": true}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["deleted"], 1);
    assert_eq!(v["purge"]["runs"], 0);

    let (st, _, v) = post(&a.app, "/api/v1/traces/delete", json!({"ids": []})).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
}

#[tokio::test]
async fn tracing_policy_by_project() {
    let mut config = Config::default();
    config.tracing.payloads = PayloadMode::Full;
    config.tracing.currency = "EUR".into();
    config.tracing.projects.insert(
        "support".into(),
        ProjectTracing {
            payloads: Some(PayloadMode::Metadata),
            max_field_bytes: None,
        },
    );
    let a = setup(Arc::new(MemoryStore::new()), config).await;
    let (st, v) = get(&a.app, "/api/v1/tracing/policy").await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        v,
        json!({"project": null, "payloads": "full", "max_field_bytes": 4096,
               "unknown_project": {"payloads": "metadata", "max_field_bytes": 4096},
               "currency": "EUR"})
    );
    let (_, v) = get(&a.app, "/api/v1/tracing/policy?project=support").await;
    assert_eq!(
        (v["project"].as_str(), v["payloads"].as_str()),
        (Some("support"), Some("metadata"))
    );
    let (_, v) = get(&a.app, "/api/v1/tracing/policy?project=other").await;
    assert_eq!(v["payloads"], "full");
}

#[cfg(unix)]
#[tokio::test]
async fn schedules_start_now_and_are_followed_by_id() {
    let mk = |name: &str, cmd: &[&str]| ScheduleConfig {
        name: name.into(),
        description: None,
        cron: "0 2 * * *".into(),
        timezone: "UTC".into(),
        command: cmd.iter().map(|s| s.to_string()).collect(),
        workdir: None,
        env: Default::default(),
        timeout: "10s".into(),
        retries: 0,
        retry_delay: "0s".into(),
        missed: Default::default(),
        enabled: true,
        keep_runs: 200,
        keep_days: 90,
        keep_output: 50,
    };
    let dir = tempfile::tempdir().unwrap();
    let config = Config {
        schedule: vec![
            mk("hello", &["sh", "-c", "echo hello from $VIGILY_SCHEDULE"]),
            mk("slow", &["sleep", "2"]),
        ],
        base_dir: dir.path().to_path_buf(),
        ..Default::default()
    };
    let a = setup(Arc::new(MemoryStore::new()), config).await;

    let (st, h, v) = post(&a.app, "/api/v1/schedules/hello/run", json!({})).await;
    assert_eq!(st, StatusCode::ACCEPTED, "{v}");
    assert_eq!(v["schedule"], "hello");
    assert_eq!(v["status"], "running");
    let id = v["id"].as_str().unwrap().to_string();
    let follow = format!("/api/v1/schedules/hello/runs/{id}");
    assert_eq!(h.get("location").unwrap(), follow.as_str());
    let mut done = Value::Null;
    for _ in 0..100 {
        let (st, v) = get(&a.app, &follow).await;
        assert_eq!(st, StatusCode::OK, "pollable from the start: {v}");
        if v["status"] != "running" {
            done = v;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(done["status"], "succeeded", "{done}");
    assert_eq!(done["stdout"], "hello from hello\n");

    let (st, _, v) = post(&a.app, "/api/v1/schedules/slow/run", json!({})).await;
    assert_eq!(st, StatusCode::ACCEPTED, "{v}");
    let (st, _, v) = post(&a.app, "/api/v1/schedules/slow/run", json!({})).await;
    assert_eq!(st, StatusCode::CONFLICT, "one run at a time: {v}");
    assert_eq!(v["error"]["code"], "conflict");

    let (st, _, v) = post(&a.app, "/api/v1/schedules/nope/run", json!({})).await;
    assert_eq!(st, StatusCode::NOT_FOUND, "{v}");
    let req = Request::post("/api/v1/schedules/hello/run")
        .header("host", "localhost")
        .header("content-type", "text/plain")
        .body(Body::empty())
        .unwrap();
    let (st, _, _) = send(&a.app, req).await;
    assert_eq!(st, StatusCode::UNSUPPORTED_MEDIA_TYPE);
}

#[tokio::test]
async fn info_lists_the_new_capabilities() {
    let a = app().await;
    let (_, v) = get(&a.app, "/api/v1/info").await;
    let caps = v["capabilities"].as_array().unwrap();
    for c in ["admin", "points"] {
        assert!(caps.contains(&json!(c)), "{c}: {v}");
    }
}
