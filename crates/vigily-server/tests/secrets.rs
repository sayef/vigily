//! Stored secrets through the API: values are write-only, scopes apply, every change is in the
//! audit trail without its value, and an engine without a key refuses to store values.

#![recursion_limit = "256"]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;
use vigily_core::{MemoryRecords, MemoryStore, RecordStore, Store};
use vigily_server::auth::AuthMode;
use vigily_server::config::Config;
use vigily_server::secrets::{KeySource, SecretKeys};
use vigily_server::{router, AppState, EngineOptions};

const ADMIN: &str = "admin-token-for-tests-only-0001";

/// A synthetic key and value, made fresh for each test.
fn synthetic(prefix: &str) -> String {
    format!(
        "{prefix}-{}",
        vigily_server::auth::new_secret() + &vigily_server::auth::new_secret()
    )
}

struct App {
    app: axum::Router,
    records: Arc<MemoryRecords>,
}

fn app(key: Option<String>) -> App {
    let store: Arc<dyn Store> = Arc::new(MemoryStore::new());
    let records = Arc::new(MemoryRecords::new());
    let state = AppState::with_options(
        store,
        records.clone(),
        Config::default(),
        EngineOptions {
            auth: AuthMode::Server {
                admin_token: ADMIN.into(),
            },
            secrets: SecretKeys {
                source: if key.is_some() {
                    KeySource::Env
                } else {
                    KeySource::None
                },
                current: key,
                previous: None,
            },
            ..Default::default()
        },
    );
    App {
        app: router(state),
        records,
    }
}

async fn send(
    a: &App,
    method: &str,
    uri: &str,
    token: &str,
    body: Option<Value>,
) -> (StatusCode, String) {
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost")
        .header("authorization", format!("Bearer {token}"));
    let req = match body {
        Some(v) => {
            b = b.header("content-type", "application/json");
            b.body(Body::from(v.to_string())).unwrap()
        }
        None => b.body(Body::empty()).unwrap(),
    };
    let res = a.app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn token(a: &App, scopes: Value) -> String {
    let (st, body) = send(
        a,
        "POST",
        "/api/v1/auth/tokens",
        ADMIN,
        Some(json!({"name": "scoped", "scopes": scopes})),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{body}");
    serde_json::from_str::<Value>(&body).unwrap()["token"]
        .as_str()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn secret_values_are_write_only_and_audited() {
    let a = app(Some(synthetic("key")));
    let value = synthetic("hook");
    let (st, body) = send(
        &a,
        "PUT",
        "/api/v1/secrets/slack.webhook",
        ADMIN,
        Some(json!({ "value": value })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert!(!body.contains(&value), "the answer holds the value");
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["name"], "slack.webhook");
    assert_eq!(v["source"], "stored");
    assert_eq!(v["changed_by"], "admin");
    assert_eq!(
        v["hint"].as_str().unwrap(),
        format!("…{}", &value[value.len() - 4..])
    );
    assert!(v.get("undecryptable").is_none());

    let (st, body) = send(&a, "GET", "/api/v1/secrets", ADMIN, None).await;
    assert_eq!(st, StatusCode::OK);
    assert!(!body.contains(&value));
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["key"], "env");
    assert_eq!(v["can_store"], true);
    assert_eq!(v["secrets"][0]["name"], "slack.webhook");

    // Stored sealed: the records hold no plain value.
    let rows = a.records.scan("").await.unwrap();
    assert!(!serde_json::to_string(&rows).unwrap().contains(&value));

    // An empty value removes it.
    let (st, body) = send(
        &a,
        "PUT",
        "/api/v1/secrets/slack.webhook",
        ADMIN,
        Some(json!({"value": ""})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["source"],
        "none"
    );

    let (st, body) = send(&a, "GET", "/api/v1/audit", ADMIN, None).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert!(!body.contains(&value));
    let v: Value = serde_json::from_str(&body).unwrap();
    let actions: Vec<_> = v["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["action"].as_str().unwrap(),
                e["target"].as_str().unwrap(),
                e["principal"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        actions,
        [
            ("secret.removed", "slack.webhook", "admin"),
            ("secret.set", "slack.webhook", "admin")
        ]
    );
}

#[tokio::test]
async fn reading_secrets_needs_read_and_writing_needs_admin() {
    let a = app(Some(synthetic("key")));
    let read = token(&a, json!(["read"])).await;
    let ingest = token(&a, json!(["ingest"])).await;
    let put = |t: String| {
        let a = &a;
        async move {
            send(
                a,
                "PUT",
                "/api/v1/secrets/x",
                &t,
                Some(json!({"value": "synthetic-value"})),
            )
            .await
            .0
        }
    };
    assert_eq!(
        send(&a, "GET", "/api/v1/secrets", &read, None).await.0,
        StatusCode::OK
    );
    assert_eq!(
        send(&a, "GET", "/api/v1/secrets", &ingest, None).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(put(read.clone()).await, StatusCode::FORBIDDEN);
    assert_eq!(put(ingest).await, StatusCode::FORBIDDEN);
    assert_eq!(
        send(&a, "GET", "/api/v1/audit", &read, None).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(put(ADMIN.to_string()).await, StatusCode::OK);
}

#[tokio::test]
async fn without_a_key_storing_is_refused_with_the_way_out() {
    let a = app(None);
    let (st, body) = send(
        &a,
        "PUT",
        "/api/v1/secrets/smtp.password",
        ADMIN,
        Some(json!({"value": "synthetic-value"})),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["error"]["code"], "secret_key_missing");
    assert!(v["error"]["message"]
        .as_str()
        .unwrap()
        .contains("VIGILY_SECRET_KEY"));
    let (_, body) = send(&a, "GET", "/api/v1/secrets", ADMIN, None).await;
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["key"], "none");
    assert_eq!(v["can_store"], false);
    assert!(v["message"].as_str().unwrap().contains("env:NAME"));
}

#[tokio::test]
async fn bad_names_and_bodies_are_refused_without_echoing_the_body() {
    let a = app(Some(synthetic("key")));
    let (st, _) = send(
        &a,
        "PUT",
        "/api/v1/secrets/no%20spaces",
        ADMIN,
        Some(json!({"value": "synthetic-value"})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, body) = send(
        &a,
        "PUT",
        "/api/v1/secrets/x",
        ADMIN,
        Some(json!({"value": ["synthetic-value-in-a-list"]})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert!(!body.contains("synthetic-value-in-a-list"), "{body}");
}

#[test]
fn keys_load_from_name_or_name_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("key");
    let key = synthetic("key");
    std::fs::write(&file, format!("{key}\n")).unwrap();
    // Names of this test only, so other tests in this process are unaffected.
    std::env::remove_var("VIGILY_TEST_KEY_A");
    std::env::set_var("VIGILY_TEST_KEY_A_FILE", &file);
    assert_eq!(
        vigily_server::secrets::env_secret("VIGILY_TEST_KEY_A").unwrap(),
        Some(key.clone())
    );
    std::env::set_var("VIGILY_TEST_KEY_A", "from-the-variable-itself");
    assert_eq!(
        vigily_server::secrets::env_secret("VIGILY_TEST_KEY_A")
            .unwrap()
            .as_deref(),
        Some("from-the-variable-itself")
    );
    std::env::remove_var("VIGILY_TEST_KEY_A");
    std::env::set_var("VIGILY_TEST_KEY_A_FILE", dir.path().join("missing"));
    assert!(vigily_server::secrets::env_secret("VIGILY_TEST_KEY_A").is_err());
    std::env::remove_var("VIGILY_TEST_KEY_A_FILE");
    assert_eq!(
        vigily_server::secrets::env_secret("VIGILY_TEST_KEY_A").unwrap(),
        None
    );
}

#[test]
fn a_local_engine_generates_its_key_file_once() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("engines").join("k.secret-key");
    let keys = SecretKeys::default().with_local_file(&file).unwrap();
    assert_eq!(keys.source, KeySource::File);
    let key = keys.current.clone().unwrap();
    assert_eq!(key.len(), 64);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
    let again = SecretKeys::default().with_local_file(&file).unwrap();
    assert_eq!(again.current, Some(key.clone()));
    // A key from the environment wins; the file's key still opens what it sealed.
    let env = SecretKeys {
        current: Some(synthetic("key")),
        previous: None,
        source: KeySource::Env,
    }
    .with_local_file(&file)
    .unwrap();
    assert_eq!(env.source, KeySource::Env);
    assert_eq!(env.previous, Some(key));
}
