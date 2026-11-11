use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;
use vigily_core::{MemoryStore, NewRun, RunStatus, Store, TrackOpts};
use vigily_server::{router, AppState};

async fn app() -> (axum::Router, Vec<String>) {
    let store = MemoryStore::new();
    let mut ids = Vec::new();
    for (i, lr) in [0.1, 0.01, 0.001, 0.0001].iter().enumerate() {
        let w = store
            .create_run(NewRun {
                name: Some(format!("sweep-{i}")),
                experiment: Some("mnist".into()),
                created_at: Some(1_000_000 * i as i64),
                kind: (i == 3).then(|| "eval".to_string()),
                target: (i == 3).then(|| json!({"model": "mlp-v2", "git": "4f2c1ab"})),
                ..Default::default()
            })
            .await
            .unwrap();
        w.set_params(
            &json!({"hparams": {"lr": lr, "batch_size": if i % 2 == 0 { 32 } else { 64 }}}),
        )
        .unwrap();
        for s in 0..5_000 {
            for subset in ["train", "val"] {
                w.track(
                    "loss",
                    1.0 / (s as f64 * lr + 1.0),
                    TrackOpts {
                        step: Some(s),
                        context: Some(json!({ "subset": subset })),
                        timestamp: Some(1_000_000 * i as i64 + s * 1000),
                        ..Default::default()
                    },
                )
                .unwrap();
            }
        }
        if i != 3 {
            w.finish(RunStatus::Finished, None).await.unwrap();
        }
        ids.push(w.info().id);
    }
    let state = AppState::new(Arc::new(store));
    (router(state), ids)
}

async fn call(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
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
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let v = serde_json::from_slice(&bytes)
        .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, v)
}

#[tokio::test]
async fn meta_lists_experiments_metrics_and_params() {
    let (app, _) = app().await;
    let (st, v) = call(&app, "GET", "/api/meta", None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["runs"], 4);
    assert_eq!(v["running"], 1);
    assert_eq!(v["kinds"], json!({"eval": 1, "training": 3}));
    assert_eq!(v["experiments"][0]["name"], "mnist");
    let keys: Vec<&str> = v["metrics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, vec!["loss[train]", "loss[val]"]);
    assert!(v["params"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["key"] == "hparams.lr" && p["kind"] == "number"));
}

#[tokio::test]
async fn runs_filter_sort_and_report_query_errors() {
    let (app, ids) = app().await;
    let (st, v) = call(
        &app,
        "GET",
        "/api/runs?q=lr%3C0.05&sort=param:hparams.lr",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let got: Vec<&str> = v["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    assert_eq!(got, vec![ids[3].as_str(), ids[2].as_str(), ids[1].as_str()]);
    assert_eq!(v["total"], 4);
    let row = &v["rows"][0];
    assert_eq!(row["metrics"]["loss[val]"]["count"], 5000);
    assert_eq!(row["params"]["hparams.batch_size"], 64);

    let (_, v) = call(
        &app,
        "GET",
        "/api/runs?q=kind:eval%20target.model=mlp-v2",
        None,
    )
    .await;
    assert_eq!(v["rows"].as_array().unwrap().len(), 1);
    assert_eq!(v["rows"][0]["kind"], "eval");
    assert_eq!(v["rows"][0]["target"]["git"], "4f2c1ab");

    let (st, v) = call(&app, "GET", "/api/runs?q=lr%3C", None).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert_eq!(v["position"], 2);
    let (st, _) = call(&app, "GET", "/api/runs?sort=bogus", None).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn run_detail_and_patch() {
    let (app, ids) = app().await;
    let (st, v) = call(&app, "GET", &format!("/api/runs/{}", ids[0]), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["params_tree"]["hparams"]["lr"], 0.1);
    assert_eq!(v["series"].as_array().unwrap().len(), 2);
    assert!(v["series"][0]["id"].is_string());

    let (st, v) = call(
        &app,
        "PATCH",
        &format!("/api/runs/{}", ids[0]),
        Some(json!({"description": "Diverged after warmup", "tags": ["keep", "keep", "lr"]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["tags"], json!(["keep", "lr"]));
    assert_eq!(v["description"], "Diverged after warmup");
    let (st, _) = call(
        &app,
        "PATCH",
        &format!("/api/runs/{}", ids[0]),
        Some(json!({"status": "failed"})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, _) = call(&app, "GET", "/api/runs/nope", None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn delete_runs_refuses_running_runs_and_removes_the_rest() {
    let (app, ids) = app().await;
    // ids[3] is still running.
    let (st, v) = call(
        &app,
        "POST",
        "/api/runs/delete",
        Some(json!({"ids": [ids[0], ids[3]]})),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{v}");
    assert!(v["error"].as_str().unwrap().contains("running"), "{v}");
    let (st, _) = call(&app, "GET", &format!("/api/runs/{}", ids[0]), None).await;
    assert_eq!(st, StatusCode::OK, "a refused request deletes nothing");

    let (st, v) = call(
        &app,
        "POST",
        "/api/runs/delete",
        Some(json!({"ids": [ids[0], ids[1]]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["deleted"], 2);
    let (st, _) = call(&app, "GET", &format!("/api/runs/{}", ids[0]), None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (_, v) = call(&app, "GET", "/api/runs", None).await;
    assert_eq!(v["total"], 2, "{v}");
    let (st, _) = call(
        &app,
        "POST",
        "/api/runs/delete",
        Some(json!({"ids": ["nope"]})),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_stale_running_run_can_be_marked_aborted_then_deleted() {
    let store = MemoryStore::new();
    let w = store
        .create_run(NewRun {
            name: Some("died".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    let id = w.info().id;
    let live = store.create_run(NewRun::default()).await.unwrap().info().id;
    let app = router(AppState::new(Arc::new(store.clone())));

    // Running and recently seen: not stale, cannot be aborted from the app.
    let (_, v) = call(&app, "GET", &format!("/api/runs/{id}"), None).await;
    assert_eq!(v["stale"], false);
    let (st, _) = call(
        &app,
        "PATCH",
        &format!("/api/runs/{id}"),
        Some(json!({"status": "aborted"})),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);

    // Quiet for ten minutes: stale; marking it aborted ends it at its last sign of life.
    let quiet = vigily_core::now_micros() - 15 * 60 * 1_000_000;
    store.set_updated_at(&id, quiet);
    // The catalog serves a snapshot up to a second old.
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let (_, v) = call(&app, "GET", &format!("/api/runs/{id}"), None).await;
    assert_eq!(
        (v["stale"].clone(), v["last_seen"].clone()),
        (json!(true), json!(quiet))
    );
    let (st, v) = call(
        &app,
        "PATCH",
        &format!("/api/runs/{id}"),
        Some(json!({"status": "aborted"})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(
        (v["status"].clone(), v["ended_at"].clone()),
        (json!("aborted"), json!(quiet))
    );
    let (st, _) = call(
        &app,
        "PATCH",
        &format!("/api/runs/{live}"),
        Some(json!({"status": "finished"})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, v) = call(&app, "POST", "/api/runs/delete", Some(json!({"ids": [id]}))).await;
    assert_eq!(st, StatusCode::OK, "{v}");
}

#[tokio::test]
async fn chart_aligns_downsamples_and_aggregates() {
    let (app, ids) = app().await;
    let req = json!({"runs": ids, "metric": "loss[val]", "points": 200});
    let (st, v) = call(&app, "POST", "/api/chart", Some(req)).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["x"].as_array().unwrap().len(), 400);
    assert_eq!(v["exact"], false);
    assert_eq!(v["series"].as_array().unwrap().len(), 4);
    assert_eq!(v["domain"], json!([0.0, 4999.0]));
    assert_eq!(v["series"][0]["last"][0], json!(4999.0));

    let zoom = json!({"runs": [ids[0]], "metric": "loss[val]", "range": [100, 199], "points": 400});
    let (_, v) = call(&app, "POST", "/api/chart", Some(zoom)).await;
    assert_eq!(v["exact"], true);
    assert_eq!(v["x"].as_array().unwrap().len(), 102);

    let agg = json!({"runs": ids, "metric": "loss[val]", "points": 100, "group_by": "param:hparams.batch_size", "aggregate": true});
    let (st, v) = call(&app, "POST", "/api/chart", Some(agg)).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let groups = v["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0]["label"], "32");
    assert_eq!(groups[0]["runs"], 2);

    let rel = json!({"runs": [ids[1], "nope"], "metric": "loss[val]", "x": "relative"});
    let (_, v) = call(&app, "POST", "/api/chart", Some(rel)).await;
    assert_eq!(v["missing"], json!(["nope"]));
    assert_eq!(v["domain"], json!([0.0, 4.999]));
}

#[tokio::test]
async fn serves_the_app_for_client_routes() {
    let (app, _) = app().await;
    let (st, v) = call(&app, "GET", "/runs/abc", None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(v.as_str().unwrap().contains("vigily"));
    let (st, _) = call(&app, "GET", "/api/nope", None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}
