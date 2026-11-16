//! Routing rules through the API (versions, validation, scopes, audit), alerts from the monitor
//! and the scheduler routed by them, the read-only channel of `[alerts] webhook`, and the guard
//! that keeps a server-mode engine from posting to internal addresses.

#![recursion_limit = "256"]

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use parking_lot::Mutex;
use serde_json::{json, Value};
use tower::ServiceExt;
use vigily_core::evals::{Alert, AlertKind, Severity};
use vigily_core::{MemoryRecords, MemoryStore, Store};
use vigily_server::auth::AuthMode;
use vigily_server::config::{Config, NotificationsConfig};
use vigily_server::delivery::DeliveryOptions;
use vigily_server::secrets::{KeySource, SecretKeys};
use vigily_server::{router, AppState, EngineOptions};

const ADMIN: &str = "admin-token-for-tests-only-0001";
const LOCAL: &str = "0123456789abcdef0123456789abcdef";

struct App {
    app: axum::Router,
    state: Arc<AppState>,
}

fn app(mode: AuthMode, config: Config) -> App {
    let store: Arc<dyn Store> = Arc::new(MemoryStore::new());
    let state = AppState::with_options(
        store,
        Arc::new(MemoryRecords::new()),
        config,
        EngineOptions {
            auth: mode,
            secrets: SecretKeys {
                current: Some(format!("key-{}", vigily_server::auth::new_secret())),
                previous: None,
                source: KeySource::Env,
            },
            delivery: DeliveryOptions {
                attempts: 2,
                base_delay: Duration::from_millis(20),
                max_delay: Duration::from_millis(50),
                timeout: Duration::from_secs(5),
            },
            ..Default::default()
        },
    );
    vigily_server::delivery::start(state.clone());
    App {
        app: router(state.clone()),
        state,
    }
}

fn server() -> AuthMode {
    AuthMode::Server {
        admin_token: ADMIN.into(),
    }
}

fn allowing() -> Config {
    Config {
        notifications: NotificationsConfig {
            allow_private_networks: true,
        },
        ..Config::default()
    }
}

async fn call(
    a: &App,
    method: &str,
    uri: &str,
    token: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
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
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn ok(a: &App, token: &str, method: &str, uri: &str, body: Option<Value>) -> Value {
    let (st, v) = call(a, method, uri, token, body).await;
    assert!(st.is_success(), "{method} {uri}: {st} {v}");
    v
}

/// A receiver on 127.0.0.1 that keeps each request's path and JSON body.
async fn receiver() -> (String, Arc<Mutex<Vec<(String, Value)>>>) {
    let got: Arc<Mutex<Vec<(String, Value)>>> = Arc::default();
    let g = got.clone();
    let app = axum::Router::new().fallback(move |req: Request<Body>| {
        let g = g.clone();
        async move {
            let path = req.uri().path().to_string();
            let body = req.into_body().collect().await.unwrap().to_bytes();
            g.lock()
                .push((path, serde_json::from_slice(&body).unwrap_or(Value::Null)));
            "ok"
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, got)
}

async fn webhook(a: &App, token: &str, name: &str, url: &str, default: bool) -> String {
    ok(
        a,
        token,
        "POST",
        "/api/v1/notifications/channels",
        Some(json!({"name": name, "kind": "webhook", "default": default,
                    "secrets": {"url": {"value": url}}})),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_string()
}

fn alert(kind: AlertKind, severity: Severity, dedupe: &str) -> Alert {
    Alert {
        id: String::new(),
        kind,
        severity,
        title: format!("{dedupe} happened"),
        detail: "what changed".into(),
        suite: (kind != AlertKind::ScheduleFailed).then(|| "916d93593f1a".into()),
        run_id: None,
        schedule: Some("nightly-intents".into()),
        created_at: 0,
        acknowledged_at: None,
        dedupe: dedupe.into(),
        delivered: None,
        deliveries: None,
        experiment: Some("intents".into()),
        suite_name: (kind != AlertKind::ScheduleFailed).then(|| "intent-clf-v2".into()),
    }
}

/// Raises `alert` and waits until its deliveries settle; returns the channel names it went to.
async fn route(a: &App, alert: Alert) -> Vec<String> {
    let dedupe = alert.dedupe.clone();
    assert!(vigily_server::monitor::notify(&a.state, alert).await);
    let id = vigily_core::evals::alerts(&*a.state.records)
        .await
        .unwrap()
        .into_iter()
        .find(|x| x.dedupe == dedupe)
        .unwrap()
        .id;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let d = ok(
            a,
            ADMIN,
            "GET",
            &format!("/api/v1/alerts/{id}/deliveries"),
            None,
        )
        .await;
        let d = d["deliveries"].as_array().unwrap().clone();
        if d.iter().all(|d| d["status"] != "pending") {
            let mut names: Vec<String> = d
                .iter()
                .map(|d| {
                    assert_eq!(d["status"], "sent", "{d}");
                    d["channel_name"].as_str().unwrap().to_string()
                })
                .collect();
            names.sort();
            return names;
        }
        assert!(Instant::now() < deadline, "still pending: {d:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn rules_are_versioned_checked_and_audited() {
    let a = app(server(), allowing());
    let (url, _) = receiver().await;
    let mail = webhook(&a, ADMIN, "mail", &url, false).await;

    let r = ok(&a, ADMIN, "GET", "/api/v1/notifications/rules", None).await;
    assert_eq!(r, json!({"version": 0, "rules": []}));

    // A rule naming a channel that does not exist, or none at all.
    for rules in [
        json!([{"match": {}, "channels": ["nope"]}]),
        json!([{"match": {"kinds": ["data_drift"]}, "channels": []}]),
    ] {
        let (st, v) = call(
            &a,
            "PUT",
            "/api/v1/notifications/rules",
            ADMIN,
            Some(json!({"version": 0, "rules": rules})),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
        assert!(v["error"]["message"].as_str().unwrap().contains("rule 1"));
    }
    let (st, _) = call(
        &a,
        "PUT",
        "/api/v1/notifications/rules",
        ADMIN,
        Some(json!({"version": 0, "rules": [{"match": {"kinds": ["no_such_kind"]}, "channels": [mail]}]})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    let body = json!({"version": 0, "rules": [
        {"name": "schedules", "match": {"kinds": ["schedule_failed"]}, "channels": [mail]}
    ]});
    let saved = ok(
        &a,
        ADMIN,
        "PUT",
        "/api/v1/notifications/rules",
        Some(body.clone()),
    )
    .await;
    assert_eq!(saved["version"], 1);
    assert_eq!(saved["changed_by"], "admin");
    assert_eq!(
        saved["rules"][0]["match"]["kinds"],
        json!(["schedule_failed"])
    );

    // A second writer at the same version loses.
    let (st, v) = call(&a, "PUT", "/api/v1/notifications/rules", ADMIN, Some(body)).await;
    assert_eq!(st, StatusCode::CONFLICT);
    assert_eq!(v["error"]["code"], "conflict");
    assert!(v["error"]["message"]
        .as_str()
        .unwrap()
        .contains("version 1"));

    // Read sees them; only admin changes them.
    let (_, t) = call(
        &a,
        "POST",
        "/api/v1/auth/tokens",
        ADMIN,
        Some(json!({"name": "reader", "scopes": ["read"]})),
    )
    .await;
    let read = t["token"].as_str().unwrap().to_string();
    let r = ok(&a, &read, "GET", "/api/v1/notifications/rules", None).await;
    assert_eq!(r["version"], 1);
    let (st, _) = call(
        &a,
        "PUT",
        "/api/v1/notifications/rules",
        &read,
        Some(json!({"version": 1, "rules": []})),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    let audit = ok(&a, ADMIN, "GET", "/api/v1/audit", None).await;
    assert_eq!(audit["entries"][0]["action"], "rules.updated");
    assert_eq!(audit["entries"][0]["target"], "rules");
}

#[tokio::test]
async fn alerts_go_where_the_rules_send_them() {
    let a = app(server(), allowing());
    let (url, _) = receiver().await;
    let pager = webhook(&a, ADMIN, "pager", &format!("{url}/pager"), false).await;
    webhook(&a, ADMIN, "slack", &format!("{url}/slack"), true).await;
    let mail = webhook(&a, ADMIN, "mail", &format!("{url}/mail"), false).await;
    ok(
        &a,
        ADMIN,
        "PUT",
        "/api/v1/notifications/rules",
        Some(json!({"version": 0, "rules": [
            {"match": {"kinds": ["schedule_failed"]}, "channels": [mail]},
            {"match": {"severities": ["critical"], "suite": "intent-*", "experiment": "int*"},
             "channels": [pager], "stop": true},
        ]})),
    )
    .await;

    // Schedule failures: the first rule and the default channels.
    assert_eq!(
        route(
            &a,
            alert(AlertKind::ScheduleFailed, Severity::Critical, "s1")
        )
        .await,
        ["mail", "slack"]
    );
    // A critical regression of a matching suite: the pager only (the rule stops).
    assert_eq!(
        route(
            &a,
            alert(AlertKind::BehaviourDrift, Severity::Critical, "b1")
        )
        .await,
        ["pager"]
    );
    // Anything else: the default channels.
    assert_eq!(
        route(&a, alert(AlertKind::DataDrift, Severity::Warning, "d1")).await,
        ["slack"]
    );
}

#[tokio::test]
async fn the_config_webhook_is_a_read_only_default_channel() {
    let (url, got) = receiver().await;
    let mut config = Config::default();
    config.alerts.webhook = Some(format!("{url}/legacy"));
    // Server mode without allow_private_networks: the operator's own URL is still trusted.
    let a = app(server(), config);
    let channels = ok(&a, ADMIN, "GET", "/api/v1/notifications/channels", None).await;
    let c = &channels["channels"][0];
    assert_eq!(c["id"], "config");
    assert_eq!(c["source"], "config");
    assert_eq!(c["default"], true);
    assert_eq!(c["secrets"]["url"]["source"], "config");
    assert!(!channels.to_string().contains(&url), "the URL is shown");
    for (method, body) in [("PATCH", Some(json!({"enabled": false}))), ("DELETE", None)] {
        let (st, v) = call(
            &a,
            method,
            "/api/v1/notifications/channels/config",
            ADMIN,
            body,
        )
        .await;
        assert_eq!(st, StatusCode::CONFLICT, "{v}");
        assert!(v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("vigily.toml"));
    }
    // Rules can name it.
    ok(
        &a,
        ADMIN,
        "PUT",
        "/api/v1/notifications/rules",
        Some(json!({"version": 0, "rules": [{"match": {}, "channels": ["config"]}]})),
    )
    .await;
    let names = route(
        &a,
        alert(AlertKind::ScheduleFailed, Severity::Critical, "legacy"),
    )
    .await;
    assert_eq!(names, ["[alerts] webhook (vigily.toml)"]);
    let body = got.lock()[0].clone();
    assert_eq!(body.0, "/legacy");
    assert_eq!(body.1["event"], "vigily.alert");
    assert_eq!(body.1["alert"]["title"], "legacy happened");
    // The alert still reports the webhook's answer, as before.
    let alerts = vigily_core::evals::alerts(&*a.state.records).await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut delivered = alerts[0].delivered;
    while delivered.is_none() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
        delivered = vigily_core::evals::alerts(&*a.state.records).await.unwrap()[0].delivered;
    }
    assert_eq!(delivered, Some(true));
    // The test route works on it too.
    let t = ok(
        &a,
        ADMIN,
        "POST",
        "/api/v1/notifications/channels/config/test",
        None,
    )
    .await;
    assert_eq!(t["ok"], true);
}

#[tokio::test]
async fn a_server_refuses_internal_targets_unless_allowed() {
    let (url, got) = receiver().await;
    // Server mode, default settings: refused after resolving, before connecting.
    let a = app(server(), Config::default());
    let id = webhook(&a, ADMIN, "internal", &format!("{url}/x"), true).await;
    let t = ok(
        &a,
        ADMIN,
        "POST",
        &format!("/api/v1/notifications/channels/{id}/test"),
        None,
    )
    .await;
    assert_eq!(t["ok"], false);
    let err = t["error"].as_str().unwrap();
    assert!(
        err.contains("127.0.0.1") && err.contains("allow_private_networks"),
        "{err}"
    );
    let host = url.replace("http://127.0.0.1", "http://localhost");
    let id = webhook(&a, ADMIN, "by-name", &format!("{host}/x"), false).await;
    let t = ok(
        &a,
        ADMIN,
        "POST",
        &format!("/api/v1/notifications/channels/{id}/test"),
        None,
    )
    .await;
    assert_eq!(t["ok"], false, "localhost resolves to loopback: {t}");
    assert!(got.lock().is_empty());

    // Allowed by the configuration flag.
    let b = app(server(), allowing());
    let id = webhook(&b, ADMIN, "internal", &format!("{url}/x"), false).await;
    let t = ok(
        &b,
        ADMIN,
        "POST",
        &format!("/api/v1/notifications/channels/{id}/test"),
        None,
    )
    .await;
    assert_eq!(t["ok"], true, "{t}");

    // A local engine (one user on a laptop) may post to localhost.
    let c = app(
        AuthMode::Local {
            token: LOCAL.into(),
        },
        Config::default(),
    );
    let id = webhook(&c, LOCAL, "local", &format!("{host}/x"), false).await;
    let t = ok(
        &c,
        LOCAL,
        "POST",
        &format!("/api/v1/notifications/channels/{id}/test"),
        None,
    )
    .await;
    assert_eq!(t["ok"], true, "{t}");
    assert_eq!(got.lock().len(), 2);
}

#[tokio::test]
async fn a_failed_schedule_run_is_routed_to_its_channels() {
    let dir = tempfile::tempdir().unwrap();
    let a = app(server(), allowing());
    let (url, got) = receiver().await;
    webhook(&a, ADMIN, "ops", &format!("{url}/ops"), true).await;
    let cfg = vigily_server::config::ScheduleConfig {
        name: "nightly-fails".into(),
        description: None,
        cron: "0 2 1 1 *".into(),
        timezone: "UTC".into(),
        command: vec!["sh".into(), "-c".into(), "exit 3".into()],
        workdir: Some(dir.path().to_path_buf()),
        env: Default::default(),
        timeout: "30s".into(),
        retries: 0,
        retry_delay: "0s".into(),
        missed: Default::default(),
        enabled: false,
        keep_runs: 200,
        keep_days: 90,
        keep_output: 50,
    };
    vigily_server::scheduler::start_now(a.state.clone(), cfg).await;
    let deadline = Instant::now() + Duration::from_secs(30);
    while got.lock().is_empty() {
        assert!(Instant::now() < deadline, "no alert delivered");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let body = got.lock()[0].1.clone();
    assert_eq!(body["alert"]["kind"], "schedule_failed");
    assert_eq!(body["alert"]["schedule"], "nightly-fails");
    assert!(body["alert"]["detail"]
        .as_str()
        .unwrap()
        .contains("status 3"));
}
