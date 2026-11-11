//! The tracing API: ingest, the payload policy, prices, the list filters, one trace, deletion.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;
use vigily_core::traces::{PayloadMode, Price, ProjectTracing};
use vigily_core::{MemoryRecords, MemoryStore, NewRun, RunStatus, Store};
use vigily_server::{config::Config, router, AppState};

async fn app(cfg: Config) -> (axum::Router, String) {
    let store = MemoryStore::new();
    let w = store
        .create_run(NewRun {
            name: Some("support-answers".into()),
            experiment: Some("support-bot".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    w.finish(RunStatus::Finished, None).await.unwrap();
    let id = w.info().id;
    let state = AppState::with(Arc::new(store), Arc::new(MemoryRecords::new()), cfg);
    state.catalog.refresh().await.unwrap();
    (router(state), id)
}

fn priced() -> Config {
    let mut c = Config::default();
    c.tracing.currency = "EUR".into();
    c.tracing.prices.insert(
        "model-a".into(),
        Price {
            input: 3.0,
            output: 15.0,
            cached_input: Some(0.3),
        },
    );
    c
}

async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<(&str, String)>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost");
    let body = match body {
        Some((ct, b)) => {
            req = req.header("content-type", ct);
            Body::from(b)
        }
        None => Body::empty(),
    };
    let res = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let v = serde_json::from_slice(&bytes)
        .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, v)
}

async fn ingest(app: &axum::Router, spans: Value) -> (StatusCode, Value) {
    call(
        app,
        "POST",
        "/api/traces/ingest",
        Some(("application/json", json!({ "spans": spans }).to_string())),
    )
    .await
}

fn spans(trace: &str, run: Option<&str>, start: i64) -> Value {
    json!([
        {"trace_id": trace, "span_id": "root", "name": "answer question", "kind": "chain",
         "start_time": start, "end_time": start + 2_000_000, "status": "ok", "run_id": run,
         "eval_item": "item-1", "input": "Question A", "output": "Answer A"},
        {"trace_id": trace, "span_id": "llm1", "parent_id": "root", "name": "chat", "kind": "llm",
         "start_time": start + 100_000, "end_time": start + 1_500_000, "status": "ok", "run_id": run,
         "model": "model-a", "provider": "example",
         "usage": {"input_tokens": 2000, "output_tokens": 500, "cached_tokens": 1000},
         "input": [{"role": "user", "content": "x".repeat(6000)}], "output": "Answer A"},
        {"trace_id": trace, "span_id": "tool1", "parent_id": "root", "name": "search", "kind": "tool",
         "start_time": start + 1_500_000, "end_time": start + 1_900_000, "status": "error",
         "error": "TimeoutError: synthetic", "run_id": run}
    ])
}

#[tokio::test]
async fn ingest_list_and_detail() {
    let (app, run) = app(priced()).await;
    let (st, v) = ingest(&app, spans("aa01", Some(&run), 1_000_000)).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["accepted"], 3);
    ingest(&app, spans("aa02", None, 5_000_000)).await;

    let (_, meta) = call(&app, "GET", "/api/meta", None).await;
    assert_eq!(meta["traces"], 2);

    let (st, v) = call(&app, "GET", "/api/traces", None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["total"], 2);
    assert_eq!(v["rows"][0]["id"], "aa02", "newest first");
    assert_eq!(v["models"], json!(["model-a"]));
    assert_eq!(v["payloads"], "truncated");
    let row = &v["rows"][1];
    assert_eq!(row["run_name"], "support-answers");
    assert_eq!(row["status"], "error");
    assert_eq!(row["duration_us"], 2_000_000);
    assert_eq!(row["currency"], "EUR");
    // 1000 × 3 + 1000 × 0.3 + 500 × 15, per million
    let want = (1000.0 * 3.0 + 1000.0 * 0.3 + 500.0 * 15.0) / 1e6;
    assert!((row["cost"].as_f64().unwrap() - want).abs() < 1e-12);
    assert!(row.get("span_ids").is_none());
    assert!((v["totals"]["cost"]["EUR"].as_f64().unwrap() - 2.0 * want).abs() < 1e-12);

    let (_, v) = call(&app, "GET", &format!("/api/traces?run={run}"), None).await;
    assert_eq!(v["total"], 1);
    let (_, v) = call(&app, "GET", "/api/traces?status=ok", None).await;
    assert_eq!(v["total"], 0);
    let (_, v) = call(&app, "GET", "/api/traces?since=4000000", None).await;
    assert_eq!(v["total"], 1);
    let (_, v) = call(&app, "GET", "/api/traces?model=model-z", None).await;
    assert_eq!(v["total"], 0);

    let (st, v) = call(&app, "GET", "/api/traces/aa01", None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["trace"]["name"], "answer question");
    assert_eq!(v["trace"]["eval_item"], "item-1");
    assert_eq!(v["spans"].as_array().unwrap().len(), 3);
    let llm = &v["spans"][1];
    assert_eq!(llm["span_id"], "llm1");
    assert_eq!(llm["payloads"], "truncated");
    assert_eq!(llm["input"].as_str().unwrap().len(), 4096);
    assert!(llm["truncated"]["input"].as_u64().unwrap() > 6000);

    let (st, _) = call(&app, "DELETE", "/api/traces/aa02", None).await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call(&app, "GET", "/api/traces/aa02", None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (_, v) = call(&app, "GET", "/api/traces", None).await;
    assert_eq!(v["total"], 1);
}

#[tokio::test]
async fn project_policy_follows_the_run_experiment() {
    let mut cfg = priced();
    cfg.tracing.payloads = PayloadMode::Full;
    cfg.tracing.projects.insert(
        "support-bot".into(),
        ProjectTracing {
            payloads: Some(PayloadMode::Metadata),
            max_field_bytes: None,
        },
    );
    let (app, run) = app(cfg).await;
    ingest(&app, spans("bb01", Some(&run), 0)).await;
    ingest(&app, spans("bb02", None, 0)).await;
    let mut other = spans("bb03", None, 0);
    for s in other.as_array_mut().unwrap() {
        s["project"] = json!("elsewhere");
    }
    ingest(&app, other).await;
    let (_, v) = call(&app, "GET", "/api/traces/bb01", None).await;
    assert_eq!(v["trace"]["payloads"], "metadata");
    assert!(v["spans"][0].get("input").is_none_or(Value::is_null));
    assert_eq!(v["spans"][2]["error"], "TimeoutError");
    // No project known: the strictest configured policy applies.
    let (_, v) = call(&app, "GET", "/api/traces/bb02", None).await;
    assert_eq!(v["trace"]["payloads"], "metadata");
    assert!(v["spans"][1].get("input").is_none_or(Value::is_null));
    // A project without its own policy gets the global one.
    let (_, v) = call(&app, "GET", "/api/traces/bb03", None).await;
    assert_eq!(v["trace"]["payloads"], "full");
    assert_eq!(
        v["spans"][1]["input"][0]["content"].as_str().unwrap().len(),
        6000
    );
}

#[tokio::test]
async fn rejects_bad_input() {
    let (app, _) = app(Config::default()).await;
    let (st, _) = ingest(
        &app,
        json!([{"trace_id": "a/b", "span_id": "x", "name": "n", "start_time": 0}]),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = call(
        &app,
        "POST",
        "/api/traces/ingest",
        Some(("application/json", "{}".into())),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = call(
        &app,
        "POST",
        "/v1/traces",
        Some(("application/x-protobuf", "\u{0}".into())),
    )
    .await;
    assert_eq!(st, StatusCode::UNSUPPORTED_MEDIA_TYPE);
}

#[tokio::test]
async fn otlp_json_export() {
    let (app, run) = app(priced()).await;
    let body = json!({"resourceSpans": [{
        "resource": {"attributes": [
            {"key": "vigily.run_id", "value": {"stringValue": run}}
        ]},
        "scopeSpans": [{"scope": {"name": "synthetic"}, "spans": [
            {"traceId": "0af7651916cd43dd8448eb211c80319c", "spanId": "b7ad6b7169203331",
             "name": "agent", "startTimeUnixNano": "1700000000000000000",
             "endTimeUnixNano": "1700000003000000000", "status": {}},
            {"traceId": "0af7651916cd43dd8448eb211c80319c", "spanId": "00f067aa0ba902b7",
             "parentSpanId": "b7ad6b7169203331", "name": "chat model-a",
             "startTimeUnixNano": "1700000000500000000", "endTimeUnixNano": "1700000002000000000",
             "attributes": [
                {"key": "gen_ai.operation.name", "value": {"stringValue": "chat"}},
                {"key": "gen_ai.request.model", "value": {"stringValue": "model-a"}},
                {"key": "gen_ai.usage.input_tokens", "value": {"intValue": "1000"}},
                {"key": "gen_ai.usage.output_tokens", "value": {"intValue": "100"}}
             ], "status": {"code": 1}}
        ]}]
    }]});
    let (st, v) = call(
        &app,
        "POST",
        "/v1/traces",
        Some(("application/json", body.to_string())),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let (_, v) = call(&app, "GET", &format!("/api/traces?run={run}"), None).await;
    assert_eq!(v["total"], 1);
    let row = &v["rows"][0];
    assert_eq!(row["name"], "agent");
    assert_eq!(row["llm_calls"], 1);
    assert_eq!(row["input_tokens"], 1000);
    assert_eq!(row["duration_us"], 3_000_000);
    assert!((row["cost"].as_f64().unwrap() - (3000.0 + 1500.0) / 1e6).abs() < 1e-12);
}

async fn post_with(app: &axum::Router, uri: &str, ct: &str, origin: Option<&str>) -> StatusCode {
    let mut req = Request::builder()
        .method("POST")
        .uri(uri)
        .header("host", "localhost:7600")
        .header("content-type", ct);
    if let Some(o) = origin {
        req = req.header("origin", o);
    }
    let body = if uri == "/v1/traces" {
        json!({"resourceSpans": []}).to_string()
    } else {
        json!({"spans": [{"trace_id": "cs01", "span_id": "a", "name": "n", "start_time": 0}]})
            .to_string()
    };
    app.clone()
        .oneshot(req.body(Body::from(body)).unwrap())
        .await
        .unwrap()
        .status()
}

/// A web page can POST `text/plain` to localhost without a preflight, and a cross-origin POST
/// carries the page's Origin: both are refused on both ingest endpoints.
#[tokio::test]
async fn ingest_refuses_requests_a_web_page_can_send() {
    let (app, _) = app(Config::default()).await;
    for uri in ["/api/traces/ingest", "/v1/traces"] {
        for ct in [
            "text/plain",
            "application/x-www-form-urlencoded",
            "multipart/form-data",
        ] {
            assert_eq!(
                post_with(&app, uri, ct, None).await,
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "{uri} {ct}"
            );
        }
        assert_eq!(
            post_with(
                &app,
                uri,
                "application/json",
                Some("https://attacker.example")
            )
            .await,
            StatusCode::FORBIDDEN,
            "{uri}"
        );
        assert_eq!(
            post_with(&app, uri, "application/json", Some("null")).await,
            StatusCode::FORBIDDEN,
            "{uri}"
        );
        assert_eq!(
            post_with(
                &app,
                uri,
                "application/json; charset=utf-8",
                Some("http://localhost:7600")
            )
            .await,
            StatusCode::OK,
            "{uri}: the app's own origin"
        );
        assert_eq!(
            post_with(&app, uri, "application/json", None).await,
            StatusCode::OK,
            "{uri}: a client"
        );
    }
}

#[tokio::test]
async fn meta_counts_new_traces_without_rescanning_and_detail_has_the_field_limit() {
    let (app, run) = app(Config::default()).await;
    let (_, meta) = call(&app, "GET", "/api/meta", None).await;
    assert_eq!(meta["traces"], 0);
    ingest(&app, spans("cc01", Some(&run), 0)).await;
    ingest(&app, spans("cc01", Some(&run), 0)).await; // resent: no new trace
    ingest(&app, spans("cc02", None, 0)).await;
    let (_, meta) = call(&app, "GET", "/api/meta", None).await;
    assert_eq!(meta["traces"], 2);
    call(&app, "DELETE", "/api/traces/cc02", None).await;
    let (_, meta) = call(&app, "GET", "/api/meta", None).await;
    assert_eq!(meta["traces"], 1);
    let (_, v) = call(&app, "GET", "/api/traces/cc01", None).await;
    assert_eq!(v["max_field_bytes"], 4096);
}
