//! The write path: creating, resuming and finishing runs, batched ingest (ordering, replays,
//! limits, the budget, deleted runs), dataset commits and the order of results and finish.

#![recursion_limit = "256"]

use std::io::Write as _;
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use http_body_util::BodyExt;
use parking_lot::Mutex;
use serde_json::{json, Value};
use tower::ServiceExt;
use vigily_core::{
    MemoryRecords, MemoryStore, NewRun, Points, RecordStore, Result as CoreResult, Run, RunPatch,
    RunStatus, RunWriter, Series, StepRange, Store, TrackOpts,
};
use vigily_server::config::Config;
use vigily_server::writers::WriteLimits;
use vigily_server::{router, AppState, EngineOptions};

const RUN: &str = "0190a0b0c0d07e0f8a9b0c1d2e3f4a5b";

fn state_with(
    store: Arc<dyn Store>,
    records: Arc<dyn RecordStore>,
    limits: WriteLimits,
) -> Arc<AppState> {
    AppState::with_options(
        store,
        records,
        Config::default(),
        EngineOptions {
            limits,
            ..Default::default()
        },
    )
}

fn memory() -> Arc<AppState> {
    state_with(
        Arc::new(MemoryStore::new()),
        Arc::new(MemoryRecords::new()),
        WriteLimits::default(),
    )
}

async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, HeaderMap, Value) {
    let res = app.clone().oneshot(req).await.unwrap();
    let (status, headers) = (res.status(), res.headers().clone());
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let v = serde_json::from_slice(&bytes)
        .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, headers, v)
}

fn post_req(uri: &str) -> axum::http::request::Builder {
    Request::post(uri)
        .header("host", "localhost")
        .header("content-type", "application/json")
}

async fn post(app: &axum::Router, uri: &str, body: Value) -> (StatusCode, HeaderMap, Value) {
    let req = post_req(uri).body(Body::from(body.to_string())).unwrap();
    send(app, req).await
}

async fn get(app: &axum::Router, uri: &str) -> (StatusCode, Value) {
    let req = Request::get(uri)
        .header("host", "localhost")
        .body(Body::empty())
        .unwrap();
    let (st, _, v) = send(app, req).await;
    (st, v)
}

async fn ingest(
    app: &axum::Router,
    batch: &Value,
    key: Option<&str>,
) -> (StatusCode, HeaderMap, Value) {
    let mut req = post_req("/api/v1/ingest");
    if let Some(k) = key {
        req = req.header("idempotency-key", k);
    }
    send(app, req.body(Body::from(batch.to_string())).unwrap()).await
}

fn points(run: &str, name: &str, steps: std::ops::Range<i64>) -> Value {
    let steps: Vec<i64> = steps.collect();
    let values: Vec<f64> = steps.iter().map(|s| 1.0 / (*s as f64 + 1.0)).collect();
    let ts: Vec<i64> = steps.iter().map(|s| 1_000_000 + s).collect();
    json!({"op": "points", "run_id": run, "name": name, "context": {"subset": "train"},
           "steps": steps, "values": values, "timestamps": ts})
}

fn batch(writer: &str, seq: u64, ops: Vec<Value>) -> Value {
    json!({"writer": writer, "seq": seq, "ops": ops})
}

async fn count(app: &axum::Router, run: &str, name: &str) -> usize {
    let (st, v) = get(
        app,
        &format!("/api/v1/runs/{run}/points?name={name}&context=%7B%22subset%22%3A%22train%22%7D"),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    v["steps"].as_array().unwrap().len()
}

#[tokio::test(flavor = "multi_thread")]
async fn one_batch_creates_writes_and_finishes_in_order() {
    let s = memory();
    let app = router(s.clone());
    let b = batch(
        "w1",
        1,
        vec![
            json!({"op": "create", "id": RUN, "name": "ordered", "experiment": "ingest"}),
            json!({"op": "params", "run_id": RUN, "params": {"hparams": {"lr": 0.01}}}),
            points(RUN, "loss", 0..100),
            json!({"op": "update", "run_id": RUN, "patch": {"tags": ["a", "b"]}}),
            json!({"op": "heartbeat", "run_id": RUN}),
            json!({"op": "finish", "run_id": RUN, "status": "finished", "ended_at": 9_000_000}),
        ],
    );
    let (st, _, r) = ingest(&app, &b, None).await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["ops"], 6);
    assert_eq!(r["runs_created"], 1);
    assert_eq!(r["points"], 100);
    assert_eq!(r["replayed"], false);
    assert_eq!(count(&app, RUN, "loss").await, 100);
    let (_, run) = get(&app, &format!("/api/v1/runs/{RUN}")).await;
    assert_eq!(run["status"], "finished");
    assert_eq!(run["ended_at"], 9_000_000);
    assert_eq!(run["tags"], json!(["a", "b"]));
    assert_eq!(run["params"]["hparams.lr"], 0.01);
    // The finished run's writer is closed.
    assert!(s.writes.writers.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn operations_on_a_run_before_its_create_fail_without_writing_later_ones() {
    let app = router(memory());
    let b = batch(
        "w1",
        1,
        vec![
            points(RUN, "loss", 0..3),
            json!({"op": "create", "id": RUN}),
        ],
    );
    let (st, _, v) = ingest(&app, &b, None).await;
    assert_eq!(st, StatusCode::GONE, "{v}");
    assert_eq!(v["error"]["code"], "run_deleted");
    assert!(v["error"]["message"]
        .as_str()
        .unwrap()
        .starts_with("operation 1 (points)"));
    let (st, _) = get(&app, &format!("/api/v1/runs/{RUN}")).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_replayed_batch_is_answered_from_the_cache_and_stores_nothing_twice() {
    let app = router(memory());
    let b = batch(
        "w1",
        1,
        vec![
            json!({"op": "create", "id": RUN}),
            points(RUN, "loss", 0..50),
        ],
    );
    let (st, _, first) = ingest(&app, &b, Some("key-1")).await;
    assert_eq!(st, StatusCode::OK, "{first}");
    // The same Idempotency-Key: the first answer, marked as replayed.
    let (st, h, again) = ingest(&app, &b, Some("key-1")).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(h["idempotent-replayed"], "true");
    assert_eq!(again, first);
    // The same writer and seq without a key: the first report, flagged replayed.
    let (_, _, by_seq) = ingest(&app, &b, None).await;
    assert_eq!(by_seq["replayed"], true);
    assert_eq!(by_seq["points"], 50);
    assert_eq!(count(&app, RUN, "loss").await, 50);
    // The next batch of the writer is applied.
    let (_, _, next) = ingest(
        &app,
        &batch("w1", 2, vec![points(RUN, "loss", 50..60)]),
        None,
    )
    .await;
    assert_eq!(next["replayed"], false);
    assert_eq!(count(&app, RUN, "loss").await, 60);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_batch_retried_after_a_restart_is_not_duplicated() {
    let dir = tempfile::tempdir().unwrap();
    let loc = dir.path().join("repo").to_string_lossy().to_string();
    let b = batch(
        "w1",
        1,
        vec![
            json!({"op": "create", "id": RUN, "name": "restart"}),
            points(RUN, "loss", 0..200),
        ],
    );
    {
        let store = vigily_core::open_store(&loc).await.unwrap();
        let records = vigily_core::open_records(&loc).await.unwrap();
        let s = state_with(store.clone(), records.clone(), WriteLimits::default());
        let app = router(s);
        let (st, _, r) = ingest(&app, &b, Some("k")).await;
        assert_eq!(st, StatusCode::OK, "{r}");
        let (st, _, _) = post(&app, "/api/v1/flush", json!({})).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(count(&app, RUN, "loss").await, 200);
        store.close().await.unwrap();
        records.flush().await.unwrap();
    }
    // A new engine remembers no batch: the retry is applied again and replaces the same points.
    let store = vigily_core::open_store(&loc).await.unwrap();
    let records = vigily_core::open_records(&loc).await.unwrap();
    let app = router(state_with(store.clone(), records, WriteLimits::default()));
    let (st, _, r) = ingest(&app, &b, Some("k")).await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["replayed"], false);
    assert_eq!(r["runs_created"], 0, "the run exists: create answers it");
    post(&app, "/api/v1/flush", json!({})).await;
    assert_eq!(count(&app, RUN, "loss").await, 200);
    let (_, run) = get(&app, &format!("/api/v1/runs/{RUN}")).await;
    assert_eq!(run["name"], "restart");
    assert_eq!(run["metrics"]["loss[train]"]["count"], 200);
    store.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn nan_and_infinities_travel_as_strings() {
    let app = router(memory());
    let b = batch(
        "w1",
        1,
        vec![
            json!({"op": "create", "id": RUN}),
            json!({"op": "points", "run_id": RUN, "name": "x", "context": {"subset": "train"},
                   "steps": [0, 1, 2, 3], "values": ["NaN", "Infinity", "-Infinity", 1.5]}),
        ],
    );
    let (st, _, r) = ingest(&app, &b, None).await;
    assert_eq!(st, StatusCode::OK, "{r}");
    let (_, v) = get(
        &app,
        &format!("/api/v1/runs/{RUN}/points?name=x&context=%7B%22subset%22%3A%22train%22%7D"),
    )
    .await;
    assert_eq!(v["values"], json!(["NaN", "Infinity", "-Infinity", 1.5]));
    // Anything else is refused.
    let bad = batch(
        "w1",
        2,
        vec![json!({"op": "points", "run_id": RUN, "name": "x", "steps": [4], "values": ["nan"]})],
    );
    let (st, _, v) = ingest(&app, &bad, None).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert_eq!(v["error"]["code"], "invalid_argument");
}

#[tokio::test(flavor = "multi_thread")]
async fn columns_of_different_lengths_are_refused() {
    let app = router(memory());
    let b = batch(
        "w1",
        1,
        vec![
            json!({"op": "create", "id": RUN}),
            json!({"op": "points", "run_id": RUN, "name": "x", "steps": [0, 1], "values": [1.0]}),
        ],
    );
    let (st, _, v) = ingest(&app, &b, None).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    assert!(v["error"]["message"]
        .as_str()
        .unwrap()
        .contains("same length"));
}

#[tokio::test(flavor = "multi_thread")]
async fn ndjson_and_gzip_bodies_are_accepted() {
    let app = router(memory());
    let lines = [
        batch("w1", 1, vec![json!({"op": "create", "id": RUN})]),
        batch("w1", 2, vec![points(RUN, "loss", 0..10)]),
    ]
    .iter()
    .map(Value::to_string)
    .collect::<Vec<_>>()
    .join("\n");
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(lines.as_bytes()).unwrap();
    let req = Request::post("/api/v1/ingest")
        .header("host", "localhost")
        .header("content-type", "application/x-ndjson")
        .header("content-encoding", "gzip")
        .body(Body::from(gz.finish().unwrap()))
        .unwrap();
    let (st, _, v) = send(&app, req).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["batches"].as_array().unwrap().len(), 2);
    assert_eq!(v["batches"][1]["points"], 10);
    assert_eq!(count(&app, RUN, "loss").await, 10);
}

#[tokio::test(flavor = "multi_thread")]
async fn bodies_over_the_limit_are_refused_after_decompression() {
    let s = state_with(
        Arc::new(MemoryStore::new()),
        Arc::new(MemoryRecords::new()),
        WriteLimits {
            max_body: 4096,
            ..Default::default()
        },
    );
    let app = router(s);
    let big = batch(
        "w1",
        1,
        vec![
            json!({"op": "create", "id": RUN}),
            points(RUN, "loss", 0..1000),
        ],
    );
    let (st, _, v) = ingest(&app, &big, None).await;
    assert_eq!(st, StatusCode::PAYLOAD_TOO_LARGE, "{v}");
    assert_eq!(v["error"]["code"], "too_large");
    // Compressed below the limit, but not after decompression.
    let padded = batch(
        "w1",
        1,
        vec![json!({"op": "params", "run_id": RUN, "params": {"note": "x".repeat(100_000)}})],
    );
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    gz.write_all(padded.to_string().as_bytes()).unwrap();
    let body = gz.finish().unwrap();
    assert!(body.len() < 4096);
    let req = Request::post("/api/v1/ingest")
        .header("host", "localhost")
        .header("content-type", "application/json")
        .header("content-encoding", "gzip")
        .body(Body::from(body))
        .unwrap();
    let (st, _, v) = send(&app, req).await;
    assert_eq!(st, StatusCode::PAYLOAD_TOO_LARGE, "{v}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_spent_budget_answers_503_with_retry_after() {
    let s = memory();
    let app = router(s.clone());
    let all = s.writes.budget.available_permits() as u32;
    let held = s.writes.budget.clone().try_acquire_many_owned(all).unwrap();
    let (st, h, v) = ingest(
        &app,
        &batch("w1", 1, vec![json!({"op": "create", "id": RUN})]),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE, "{v}");
    assert_eq!(v["error"]["code"], "overloaded");
    assert_eq!(h["retry-after"], "1");
    drop(held);
    let (st, _, _) = ingest(
        &app,
        &batch("w1", 1, vec![json!({"op": "create", "id": RUN})]),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
}

#[tokio::test(flavor = "multi_thread")]
async fn writes_to_a_deleted_run_answer_run_deleted() {
    let app = router(memory());
    let b = batch(
        "w1",
        1,
        vec![
            json!({"op": "create", "id": RUN}),
            points(RUN, "loss", 0..5),
            json!({"op": "finish", "run_id": RUN, "status": "finished"}),
        ],
    );
    assert_eq!(ingest(&app, &b, None).await.0, StatusCode::OK);
    let (st, _, v) = post(&app, "/api/v1/runs/delete", json!({"ids": [RUN]})).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let (st, _, v) = ingest(&app, &batch("w1", 2, vec![points(RUN, "loss", 5..6)]), None).await;
    assert_eq!(st, StatusCode::GONE, "{v}");
    assert_eq!(v["error"]["code"], "run_deleted");
    let (st, _, v) = post(
        &app,
        &format!("/api/v1/runs/{RUN}/finish"),
        json!({"status": "failed"}),
    )
    .await;
    assert_eq!(st, StatusCode::GONE);
    assert_eq!(v["error"]["code"], "run_deleted");
}

#[tokio::test(flavor = "multi_thread")]
async fn create_with_an_existing_id_answers_the_existing_run() {
    let app = router(memory());
    let (st, _, v) = post(&app, "/api/v1/runs", json!({"id": RUN, "name": "first"})).await;
    assert_eq!(st, StatusCode::CREATED, "{v}");
    assert_eq!(v["created"], true);
    assert_eq!(v["run"]["name"], "first");
    let (st, _, v) = post(&app, "/api/v1/runs", json!({"id": RUN, "name": "second"})).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["created"], false);
    assert_eq!(v["run"]["name"], "first");
    let (st, _, v) = post(&app, "/api/v1/runs", json!({"id": "not-a-uuid"})).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert!(v["error"]["message"].as_str().unwrap().contains("UUIDv7"));
}

#[tokio::test(flavor = "multi_thread")]
async fn resume_answers_params_and_the_last_step_of_each_series() {
    let store: Arc<dyn Store> = Arc::new(MemoryStore::new());
    let w = store
        .create_run(NewRun {
            id: Some(RUN.into()),
            name: Some("earlier".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    w.set_params(&json!({"lr": 0.1})).unwrap();
    for step in [0, 5, 9] {
        w.track(
            "loss",
            1.0,
            TrackOpts {
                step: Some(step),
                ..Default::default()
            },
        )
        .unwrap();
    }
    w.finish(RunStatus::Failed, None).await.unwrap();
    let app = router(state_with(
        store,
        Arc::new(MemoryRecords::new()),
        WriteLimits::default(),
    ));
    let req = Request::post(format!("/api/v1/runs/{RUN}/resume"))
        .header("host", "localhost")
        .body(Body::empty())
        .unwrap();
    let (st, _, v) = send(&app, req).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["run"]["name"], "earlier");
    assert_eq!(v["params"], json!({"lr": 0.1}));
    assert_eq!(v["series"][0]["key"], "loss");
    assert_eq!(v["series"][0]["last_step"], 9);
    let (st, _, v) = post(
        &app,
        "/api/v1/runs/0190a0b0c0d07e0f8a9b0c1d2e3f4a5c/resume",
        json!({}),
    )
    .await;
    assert_eq!(st, StatusCode::GONE, "{v}");
}

#[tokio::test(flavor = "multi_thread")]
async fn dataset_commits_need_an_idempotency_key_and_replay_it() {
    let app = router(memory());
    let body =
        json!({"items": [{"input": "2+2", "expected": "4"}, {"input": "3+3", "expected": "6"}]});
    let (st, _, v) = post(&app, "/api/v1/datasets/math/versions", body.clone()).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert!(v["error"]["message"]
        .as_str()
        .unwrap()
        .contains("Idempotency-Key"));
    let commit = |key: &'static str, body: Value| {
        let app = app.clone();
        async move {
            let req = post_req("/api/v1/datasets/math/versions")
                .header("idempotency-key", key)
                .body(Body::from(body.to_string()))
                .unwrap();
            send(&app, req).await
        }
    };
    let (st, _, first) = commit("c1", body.clone()).await;
    assert_eq!(st, StatusCode::CREATED, "{first}");
    assert_eq!(first["version"], 1);
    let (st, h, again) = commit("c1", body.clone()).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(h["idempotent-replayed"], "true");
    assert_eq!(again, first);
    let more = json!({"items": [{"input": "4+4", "expected": "8"}], "mode": "upsert"});
    let (st, _, second) = commit("c2", more.clone()).await;
    assert_eq!(st, StatusCode::CREATED);
    assert_eq!(second["version"], 2);
    // A retry of the first commit still answers version 1.
    assert_eq!(commit("c1", body).await.2["version"], 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn writes_from_web_pages_of_other_origins_are_refused() {
    let app = router(memory());
    let req = post_req("/api/v1/ingest")
        .header("origin", "http://evil.example")
        .body(Body::from(batch("w", 1, vec![]).to_string()))
        .unwrap();
    let (st, _, v) = send(&app, req).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{v}");
    let req = Request::post("/api/v1/runs")
        .header("host", "localhost")
        .header("content-type", "text/plain")
        .body(Body::from(json!({"id": RUN}).to_string()))
        .unwrap();
    assert_eq!(send(&app, req).await.0, StatusCode::UNSUPPORTED_MEDIA_TYPE);
}

// ---------------------------------------------------------------------------------------------
// Results are in storage before the run is finished.

type Log = Arc<Mutex<Vec<String>>>;

struct LoggedRecords {
    inner: MemoryRecords,
    log: Log,
}

#[async_trait]
impl RecordStore for LoggedRecords {
    async fn put(&self, rows: Vec<(String, Value)>) -> CoreResult<()> {
        self.log.lock().push("records.put".into());
        self.inner.put(rows).await
    }
    async fn get(&self, key: &str) -> CoreResult<Option<Value>> {
        self.inner.get(key).await
    }
    async fn scan(&self, prefix: &str) -> CoreResult<Vec<(String, Value)>> {
        self.inner.scan(prefix).await
    }
    async fn delete(&self, keys: Vec<String>) -> CoreResult<()> {
        self.inner.delete(keys).await
    }
    async fn flush(&self) -> CoreResult<()> {
        self.log.lock().push("records.flush".into());
        self.inner.flush().await
    }
    async fn refresh(&self) -> CoreResult<()> {
        self.inner.refresh().await
    }
}

struct LoggedStore {
    inner: MemoryStore,
    log: Log,
}

struct LoggedWriter {
    inner: Box<dyn RunWriter>,
    log: Log,
}

#[async_trait]
impl RunWriter for LoggedWriter {
    fn info(&self) -> Run {
        self.inner.info()
    }
    fn track(&self, name: &str, value: f64, opts: TrackOpts) -> CoreResult<()> {
        self.inner.track(name, value, opts)
    }
    fn set_params(&self, params: &Value) -> CoreResult<()> {
        self.inner.set_params(params)
    }
    fn set_param(&self, key: &str, value: Value) -> CoreResult<()> {
        self.inner.set_param(key, value)
    }
    async fn update(&self, patch: RunPatch) -> CoreResult<()> {
        self.inner.update(patch).await
    }
    async fn finish(&self, status: RunStatus, ended_at: Option<i64>) -> CoreResult<()> {
        self.log.lock().push("run.finish".into());
        self.inner.finish(status, ended_at).await
    }
    async fn flush(&self) -> CoreResult<()> {
        self.inner.flush().await
    }
}

#[async_trait]
impl Store for LoggedStore {
    fn location(&self) -> String {
        self.inner.location()
    }
    async fn runs(&self) -> CoreResult<Vec<Run>> {
        self.inner.runs().await
    }
    async fn params(&self, run_id: &str) -> CoreResult<Value> {
        self.inner.params(run_id).await
    }
    async fn series(&self, run_id: &str) -> CoreResult<Vec<Series>> {
        self.inner.series(run_id).await
    }
    async fn points(
        &self,
        ids: &[u64],
        range: StepRange,
    ) -> CoreResult<std::collections::HashMap<u64, Points>> {
        self.inner.points(ids, range).await
    }
    async fn delete_runs(&self, ids: &[String]) -> CoreResult<usize> {
        self.inner.delete_runs(ids).await
    }
    async fn create_run(&self, new: NewRun) -> CoreResult<Box<dyn RunWriter>> {
        Ok(Box::new(LoggedWriter {
            inner: self.inner.create_run(new).await?,
            log: self.log.clone(),
        }))
    }
    async fn open_run(&self, id: &str) -> CoreResult<Box<dyn RunWriter>> {
        Ok(Box::new(LoggedWriter {
            inner: self.inner.open_run(id).await?,
            log: self.log.clone(),
        }))
    }
    async fn update_run(&self, id: &str, patch: RunPatch) -> CoreResult<Run> {
        self.inner.update_run(id, patch).await
    }
    async fn flush(&self) -> CoreResult<()> {
        self.inner.flush().await
    }
    async fn close(&self) -> CoreResult<()> {
        self.inner.close().await
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn results_and_features_are_flushed_before_the_run_is_finished() {
    let log: Log = Arc::default();
    let store = Arc::new(LoggedStore {
        inner: MemoryStore::new(),
        log: log.clone(),
    });
    let records = Arc::new(LoggedRecords {
        inner: MemoryRecords::new(),
        log: log.clone(),
    });
    let app = router(state_with(store, records.clone(), WriteLimits::default()));
    let result = json!({"run_id": RUN, "item_id": "i1", "item_hash": "h", "dataset": "math",
        "dataset_version": 1, "output": "4", "error": null, "latency_ms": 1.0, "scores": [],
        "created_at": 1});
    let feature = json!({"run_id": RUN, "feature": "length", "values": [1, 2, 3]});
    let b = batch(
        "w1",
        1,
        vec![
            json!({"op": "create", "id": RUN, "kind": "eval"}),
            json!({"op": "results", "results": [result]}),
            json!({"op": "features", "samples": [feature]}),
            json!({"op": "finish", "run_id": RUN, "status": "finished"}),
        ],
    );
    let (st, _, r) = ingest(&app, &b, None).await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(
        (r["results"].as_u64(), r["features"].as_u64()),
        (Some(1), Some(1))
    );
    let log = log.lock().clone();
    let finish = log.iter().position(|e| e == "run.finish").unwrap();
    let last_put = log.iter().rposition(|e| e == "records.put").unwrap();
    let flush = log[..finish].iter().rposition(|e| e == "records.flush");
    assert!(
        flush.is_some_and(|f| f > last_put),
        "records must be flushed after the last put and before finish: {log:?}"
    );
    assert_eq!(records.inner.scan("res/").await.unwrap().len(), 1);
    assert_eq!(records.inner.scan("feat/").await.unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn rest_routes_store_results_and_finish() {
    let app = router(memory());
    post(&app, "/api/v1/runs", json!({"id": RUN, "kind": "eval"})).await;
    let result = json!({"run_id": RUN, "item_id": "i1", "item_hash": "h", "dataset": "math",
        "dataset_version": 1, "output": "4", "error": null, "latency_ms": 1.0, "scores": [],
        "created_at": 1});
    let (st, _, v) = post(
        &app,
        &format!("/api/v1/runs/{RUN}/results"),
        json!({"results": [result]}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["stored"], 1);
    let (st, _, _) = post(
        &app,
        &format!("/api/v1/runs/{RUN}/results"),
        json!({"results": [{"run_id": "other", "item_id": "i", "item_hash": "h", "dataset": "d",
            "dataset_version": 1, "output": null, "error": null, "latency_ms": null, "scores": [],
            "created_at": 1}]}),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _, v) = post(
        &app,
        &format!("/api/v1/runs/{RUN}/features"),
        json!({"samples": [{"run_id": RUN, "feature": "f", "values": ["a", "b"]}]}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let (st, _, v) = post(
        &app,
        &format!("/api/v1/runs/{RUN}/finish"),
        json!({"status": "running"}),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    let (st, _, v) = post(
        &app,
        &format!("/api/v1/runs/{RUN}/finish"),
        json!({"status": "finished", "ended_at": 5}),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["status"], "finished");
    assert_eq!(v["ended_at"], 5);
    let (st, _, v) = post(&app, "/api/v1/flush", json!({})).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert!(v["refreshed_at"].as_i64().unwrap() > 0);
}
