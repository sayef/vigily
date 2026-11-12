//! The routes of earlier releases under `/api/*` stay as aliases of `/api/v1`: the same answers,
//! a `Deprecation` header and a `Link` to the successor, and the flat `{"error": "..."}` body.
//! `/api/v1` answers errors as `{"error": {"code", "message", "position"?}}`.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;
use vigily_core::{MemoryStore, NewRun, RunStatus, Store, TrackOpts};
use vigily_server::{router, AppState};

async fn app() -> (axum::Router, String) {
    let store = MemoryStore::new();
    let w = store
        .create_run(NewRun {
            name: Some("baseline".into()),
            experiment: Some("mnist".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    for s in 0..10 {
        w.track("loss", 1.0 / (s as f64 + 1.0), TrackOpts::default())
            .unwrap();
    }
    w.finish(RunStatus::Finished, None).await.unwrap();
    let id = w.info().id;
    (router(AppState::new(Arc::new(store))), id)
}

async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, HeaderMap, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost");
    let body = match body {
        Some(b) => {
            req = req.header("content-type", "application/json");
            Body::from(b.to_string())
        }
        None => Body::empty(),
    };
    let res = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let (status, headers) = (res.status(), res.headers().clone());
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let v = serde_json::from_slice(&bytes)
        .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, headers, v)
}

fn header<'a>(h: &'a HeaderMap, name: &str) -> Option<&'a str> {
    h.get(name).and_then(|v| v.to_str().ok())
}

#[tokio::test]
async fn aliases_answer_like_v1_with_deprecation_headers() {
    let (app, id) = app().await;
    for path in [
        "/meta".to_string(),
        "/runs?q=&sort=run:name".to_string(),
        format!("/runs/{id}"),
        "/datasets".to_string(),
        "/schedules".to_string(),
        "/suites".to_string(),
        "/alerts".to_string(),
    ] {
        let (st, h, old) = call(&app, "GET", &format!("/api{path}"), None).await;
        assert_eq!(st, StatusCode::OK, "{path}: {old}");
        assert_eq!(header(&h, "deprecation"), Some("true"), "{path}");
        assert_eq!(
            header(&h, "link"),
            Some(format!("</api/v1{path}>; rel=\"successor-version\"").as_str()),
            "{path}"
        );
        let (st, h, new) = call(&app, "GET", &format!("/api/v1{path}"), None).await;
        assert_eq!(st, StatusCode::OK, "{path}: {new}");
        assert!(header(&h, "deprecation").is_none(), "{path}");
        // `refreshed_at` and durations move with the clock; everything else is the same.
        let strip = |mut v: Value| {
            if let Some(o) = v.as_object_mut() {
                o.remove("refreshed_at");
                o.remove("duration");
            }
            if let Some(rows) = v.get_mut("rows").and_then(Value::as_array_mut) {
                for r in rows {
                    r.as_object_mut().unwrap().remove("duration");
                }
            }
            v
        };
        assert_eq!(strip(old), strip(new), "{path}");
    }
}

#[tokio::test]
async fn health_answers_on_both() {
    let (app, _) = app().await;
    let (st, _, v) = call(&app, "GET", "/api/v1/health", None).await;
    assert_eq!((st, v), (StatusCode::OK, json!("ok")));
    let (st, h, v) = call(&app, "GET", "/api/health", None).await;
    assert_eq!((st, v), (StatusCode::OK, json!("ok")));
    assert_eq!(header(&h, "deprecation"), Some("true"));
}

#[tokio::test]
async fn info_names_the_api_version_mode_and_capabilities() {
    let (app, _) = app().await;
    let (st, _, v) = call(&app, "GET", "/api/v1/info", None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["api_version"], "v1");
    assert_eq!(v["mode"], "local");
    assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
    let caps: Vec<&str> = v["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(caps.contains(&"runs") && caps.contains(&"traces"), "{v}");
    // New routes have no alias.
    let (st, _, _) = call(&app, "GET", "/api/info", None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn errors_are_structured_on_v1_and_flat_on_aliases() {
    let (app, _) = app().await;

    let (st, _, v) = call(&app, "GET", "/api/v1/runs/nope", None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    assert_eq!(
        v,
        json!({"error": {"code": "not_found", "message": "no run with id nope"}})
    );
    let (st, _, v) = call(&app, "GET", "/api/runs/nope", None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    assert_eq!(v, json!({"error": "no run with id nope"}));

    let (st, _, v) = call(&app, "GET", "/api/v1/runs?q=lr%3C", None).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert_eq!(v["error"]["code"], "invalid_argument");
    assert_eq!(v["error"]["position"], 2);
    let (_, _, v) = call(&app, "GET", "/api/runs?q=lr%3C", None).await;
    assert_eq!(v["position"], 2);
    assert!(v["error"].is_string(), "{v}");

    // A running run cannot be deleted: a conflict.
    let store = MemoryStore::new();
    let w = store.create_run(NewRun::default()).await.unwrap();
    let running = w.info().id;
    let app2 = router(AppState::new(Arc::new(store)));
    let (st, _, v) = call(
        &app2,
        "POST",
        "/api/v1/runs/delete",
        Some(json!({"ids": [running]})),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
    assert_eq!(v["error"]["code"], "conflict");

    // Extractor rejections get the structured shape on v1 and stay as they were on aliases.
    let req = |uri: &str| {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header("host", "localhost")
            .header("content-type", "application/json")
            .body(Body::from("{not json"))
            .unwrap()
    };
    let res = app.clone().oneshot(req("/api/v1/chart")).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let v: Value =
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(v["error"]["code"], "invalid_argument");
    assert!(!v["error"]["message"].as_str().unwrap().is_empty());
    let res = app.clone().oneshot(req("/api/chart")).await.unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);

    // Unknown routes.
    let (st, _, v) = call(&app, "GET", "/api/v1/nope", None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    assert_eq!(v["error"]["code"], "not_found");
    let (st, _, v) = call(&app, "GET", "/api/nope", None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    assert!(v["error"].is_string(), "{v}");
}

#[tokio::test]
async fn writes_work_through_aliases() {
    let (app, id) = app().await;
    let (st, h, v) = call(
        &app,
        "PATCH",
        &format!("/api/runs/{id}"),
        Some(json!({"name": "renamed"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["name"], "renamed");
    assert_eq!(
        header(&h, "link"),
        Some(format!("</api/v1/runs/{id}>; rel=\"successor-version\"").as_str())
    );
    let (st, _, v) = call(
        &app,
        "POST",
        "/api/chart",
        Some(json!({"runs": [id], "metric": "loss"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["series"][0]["y"].as_array().unwrap().len(), 10);
}

#[tokio::test]
async fn otlp_stays_at_the_root() {
    let (app, _) = app().await;
    let (st, h, v) = call(
        &app,
        "POST",
        "/v1/traces",
        Some(json!({"resourceSpans": []})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert!(header(&h, "deprecation").is_none());
    let (st, _, _) = call(
        &app,
        "POST",
        "/api/v1/v1/traces",
        Some(json!({"resourceSpans": []})),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}
