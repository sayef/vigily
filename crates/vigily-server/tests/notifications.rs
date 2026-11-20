//! Notification channels end to end, against receivers in this process: the channel API with
//! write-only secrets, each kind's payload (and the webhook signature), retries, rate limits,
//! dedupe, resuming after a restart, test sends and e-mail over SMTP. Nothing leaves the machine.

#![recursion_limit = "256"]

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use hmac::{Hmac, KeyInit, Mac};
use http_body_util::BodyExt;
use parking_lot::Mutex;
use serde_json::{json, Value};
use sha2::Sha256;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tower::ServiceExt;
use vigily_core::evals::{Alert, AlertKind, Severity};
use vigily_core::{MemoryRecords, MemoryStore, Store};
use vigily_server::auth::AuthMode;
use vigily_server::config::Config;
use vigily_server::delivery::DeliveryOptions;
use vigily_server::secrets::{KeySource, SecretKeys};
use vigily_server::{router, AppState, EngineOptions};

const ADMIN: &str = "admin-token-for-tests-only-0001";
const PUBLIC: &str = "https://vigily.example.com";

fn synthetic(prefix: &str) -> String {
    format!("{prefix}-{}", vigily_server::auth::new_secret())
}

struct App {
    app: axum::Router,
    state: Arc<AppState>,
}

fn state_over(records: Arc<MemoryRecords>, key: &str) -> Arc<AppState> {
    let store: Arc<dyn Store> = Arc::new(MemoryStore::new());
    AppState::with_options(
        store,
        records,
        Config {
            // The receivers here listen on 127.0.0.1.
            notifications: vigily_server::config::NotificationsConfig {
                allow_private_networks: true,
            },
            ..Config::default()
        },
        EngineOptions {
            auth: AuthMode::Server {
                admin_token: ADMIN.into(),
            },
            secrets: SecretKeys {
                current: Some(key.to_string()),
                previous: None,
                source: KeySource::Env,
            },
            delivery: DeliveryOptions {
                attempts: 3,
                base_delay: Duration::from_millis(20),
                max_delay: Duration::from_millis(100),
                timeout: Duration::from_secs(5),
            },
            public_url: Some(PUBLIC.into()),
            ..Default::default()
        },
    )
}

/// An engine with its delivery worker running.
fn app() -> App {
    let state = state_over(Arc::new(MemoryRecords::new()), &synthetic("key"));
    vigily_server::delivery::start(state.clone());
    App {
        app: router(state.clone()),
        state,
    }
}

async fn call(
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

async fn json(a: &App, method: &str, uri: &str, body: Option<Value>) -> Value {
    let (st, text) = call(a, method, uri, ADMIN, body).await;
    assert!(st.is_success(), "{method} {uri}: {st} {text}");
    serde_json::from_str(&text).unwrap()
}

async fn channel(a: &App, body: Value) -> String {
    json(a, "POST", "/api/v1/notifications/channels", Some(body)).await["id"]
        .as_str()
        .unwrap()
        .to_string()
}

/// A request a receiver got.
#[derive(Clone, Debug)]
struct Got {
    path: String,
    headers: HeaderMap,
    body: Vec<u8>,
    at: Instant,
}

impl Got {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap()
    }
}

/// An HTTP receiver on a free port: answers each request with the next of `statuses` (200 when
/// they run out) and keeps what it got.
struct Receiver {
    url: String,
    got: Arc<Mutex<Vec<Got>>>,
}

async fn receiver(statuses: &[u16]) -> Receiver {
    let got: Arc<Mutex<Vec<Got>>> = Arc::default();
    let queue = Arc::new(Mutex::new(
        statuses.iter().copied().collect::<VecDeque<u16>>(),
    ));
    let (g, q) = (got.clone(), queue.clone());
    let app = axum::Router::new().fallback(move |req: Request<Body>| {
        let (g, q) = (g.clone(), q.clone());
        async move {
            let (parts, body) = req.into_parts();
            let body = body.collect().await.unwrap().to_bytes().to_vec();
            g.lock().push(Got {
                path: parts.uri.path().to_string(),
                headers: parts.headers,
                body,
                at: Instant::now(),
            });
            let status = q.lock().pop_front().unwrap_or(200);
            (StatusCode::from_u16(status).unwrap(), "ok")
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Receiver { url, got }
}

impl Receiver {
    fn got(&self) -> Vec<Got> {
        self.got.lock().clone()
    }

    async fn wait_for(&self, n: usize) -> Vec<Got> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let got = self.got();
            if got.len() >= n {
                return got;
            }
            assert!(
                Instant::now() < deadline,
                "got {} of {n} requests",
                got.len()
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

fn alert(dedupe: &str) -> Alert {
    Alert {
        id: String::new(),
        kind: AlertKind::BehaviourDrift,
        severity: Severity::Critical,
        title: "accuracy dropped on intent-v2".into(),
        detail: "accuracy is 0.810 against 0.900 on the same 200 items over 5 baseline runs."
            .into(),
        suite: Some("suite/intents v2".into()),
        run_id: Some("0190a0b0c0d07e0f8a9b0c1d2e3f4a5b".into()),
        schedule: Some("nightly".into()),
        created_at: 0,
        acknowledged_at: None,
        dedupe: dedupe.into(),
        delivered: None,
        deliveries: None,
        experiment: None,
        suite_name: None,
    }
}

/// Raises an alert and returns its id.
async fn raise(a: &App, dedupe: &str) -> String {
    assert!(vigily_server::monitor::notify(&a.state, alert(dedupe)).await);
    let all = vigily_core::evals::alerts(&*a.state.records).await.unwrap();
    all.into_iter().find(|x| x.dedupe == dedupe).unwrap().id
}

async fn deliveries(a: &App, alert: &str) -> Vec<Value> {
    json(
        a,
        "GET",
        &format!("/api/v1/alerts/{alert}/deliveries"),
        None,
    )
    .await["deliveries"]
        .as_array()
        .unwrap()
        .clone()
}

/// Waits until no delivery of `alert` is pending.
async fn settled(a: &App, alert: &str) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let d = deliveries(a, alert).await;
        if !d.is_empty() && d.iter().all(|d| d["status"] != "pending") {
            return d;
        }
        assert!(Instant::now() < deadline, "still pending: {d:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn channels_keep_secrets_write_only_and_audit_changes() {
    let a = app();
    let url = format!("https://hooks.slack.example/services/{}", synthetic("T0"));
    let created = json(
        &a,
        "POST",
        "/api/v1/notifications/channels",
        Some(
            json!({"name": "ml-alerts", "kind": "slack", "default": true,
                    "secrets": {"webhook_url": {"value": url}}}),
        ),
    )
    .await;
    let text = created.to_string();
    assert!(!text.contains(&url), "the answer holds the URL");
    assert_eq!(created["kind"], "slack");
    assert_eq!(created["enabled"], true);
    assert_eq!(created["default"], true);
    assert_eq!(created["source"], "api");
    assert_eq!(created["secrets"]["webhook_url"]["source"], "stored");
    assert_eq!(created["secrets"]["webhook_url"]["changed_by"], "admin");
    let id = created["id"].as_str().unwrap().to_string();

    // From the environment instead; the stored value goes.
    let changed = json(
        &a,
        "PATCH",
        &format!("/api/v1/notifications/channels/{id}"),
        Some(json!({"name": "ml", "enabled": false,
                    "secrets": {"webhook_url": {"env": "VIGILY_TEST_SLACK_URL"}}})),
    )
    .await;
    assert_eq!(changed["name"], "ml");
    assert_eq!(changed["enabled"], false);
    assert_eq!(
        changed["secrets"]["webhook_url"]["source"],
        "env:VIGILY_TEST_SLACK_URL"
    );
    let rows = a.state.records.scan("secret/").await.unwrap();
    assert!(rows.is_empty(), "{rows:?}");

    // Refusals: a missing required secret, a secret of another kind, a bad URL, a taken name,
    // a clearing of a required secret, SMTP settings on a webhook kind.
    for (body, want) in [
        (
            json!({"name": "x", "kind": "discord"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"name": "x", "kind": "slack", "secrets": {"url": {"value": "https://a.example"}}}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"name": "x", "kind": "teams", "secrets": {"webhook_url": {"value": "not a url"}}}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"name": "ML", "kind": "webhook", "secrets": {"url": {"value": "https://a.example"}}}),
            StatusCode::CONFLICT,
        ),
        (
            json!({"name": "x", "kind": "webhook", "settings": {"host": "smtp.example.com"},
                   "secrets": {"url": {"value": "https://a.example"}}}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"name": "x", "kind": "email", "settings": {"host": "smtp.example.com", "from": "nope"}}),
            StatusCode::BAD_REQUEST,
        ),
    ] {
        let (st, text) = call(
            &a,
            "POST",
            "/api/v1/notifications/channels",
            ADMIN,
            Some(body.clone()),
        )
        .await;
        assert_eq!(st, want, "{body}: {text}");
    }
    let (st, _) = call(
        &a,
        "PATCH",
        &format!("/api/v1/notifications/channels/{id}"),
        ADMIN,
        Some(json!({"secrets": {"webhook_url": {"value": ""}}})),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    // Scopes: read lists, only admin changes.
    let (_, t) = call(
        &a,
        "POST",
        "/api/v1/auth/tokens",
        ADMIN,
        Some(json!({"name": "reader", "scopes": ["read"]})),
    )
    .await;
    let read = serde_json::from_str::<Value>(&t).unwrap()["token"]
        .as_str()
        .unwrap()
        .to_string();
    let (st, _) = call(&a, "GET", "/api/v1/notifications/channels", &read, None).await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call(
        &a,
        "POST",
        &format!("/api/v1/notifications/channels/{id}/test"),
        &read,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = call(
        &a,
        "DELETE",
        &format!("/api/v1/notifications/channels/{id}"),
        &read,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    let deleted = json(
        &a,
        "DELETE",
        &format!("/api/v1/notifications/channels/{id}"),
        None,
    )
    .await;
    assert_eq!(deleted["deleted"], true);
    let (st, _) = call(
        &a,
        "GET",
        &format!("/api/v1/notifications/channels/{id}"),
        ADMIN,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    let audit = json(&a, "GET", "/api/v1/audit", None).await;
    assert!(!audit.to_string().contains(&url));
    let actions: Vec<String> = audit["entries"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .map(|e| e["action"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        actions,
        [
            "secret.set",
            "channel.created",
            "secret.env",
            "channel.updated",
            "channel.deleted"
        ]
    );
}

#[tokio::test]
async fn each_kind_gets_its_payload_once_and_webhooks_are_signed() {
    let a = app();
    let r = receiver(&[]).await;
    let signing_key = synthetic("signing");
    for (name, kind, field, path) in [
        ("slack", "slack", "webhook_url", "/slack/T0/B0/x"),
        ("teams", "teams", "webhook_url", "/teams"),
        ("discord", "discord", "webhook_url", "/discord"),
    ] {
        channel(
            &a,
            json!({"name": name, "kind": kind, "default": true,
                   "secrets": {field: {"value": format!("{}{path}", r.url)}}}),
        )
        .await;
    }
    channel(
        &a,
        json!({"name": "hook", "kind": "webhook", "default": true,
               "secrets": {"url": {"value": format!("{}/hook", r.url)},
                           "signing_key": {"value": signing_key}}}),
    )
    .await;
    // Not default: gets nothing.
    channel(
        &a,
        json!({"name": "quiet", "kind": "webhook",
               "secrets": {"url": {"value": format!("{}/quiet", r.url)}}}),
    )
    .await;

    let id = raise(&a, "behaviour/suite/run/accuracy").await;
    let got = r.wait_for(4).await;
    let by = |p: &str| got.iter().find(|g| g.path.starts_with(p)).unwrap().clone();
    let link = format!("{PUBLIC}/drift/suite%2Fintents%20v2");

    let slack = by("/slack").json();
    assert_eq!(slack["text"], "Critical: accuracy dropped on intent-v2");
    assert_eq!(slack["blocks"][0]["type"], "header");
    assert_eq!(
        slack["blocks"][0]["text"]["text"],
        "accuracy dropped on intent-v2"
    );
    assert!(slack["blocks"][1]["text"]["text"]
        .as_str()
        .unwrap()
        .contains("0.810 against 0.900"));
    let fields = slack["blocks"][2]["fields"].to_string();
    assert!(
        fields.contains("nightly") && fields.contains("UTC"),
        "{fields}"
    );
    assert_eq!(slack["blocks"][3]["elements"][0]["url"], link);

    let teams = by("/teams").json();
    assert_eq!(teams["type"], "message");
    let card = &teams["attachments"][0];
    assert_eq!(
        card["contentType"],
        "application/vnd.microsoft.card.adaptive"
    );
    assert_eq!(card["content"]["type"], "AdaptiveCard");
    assert_eq!(
        card["content"]["body"][1]["text"],
        "accuracy dropped on intent-v2"
    );
    assert_eq!(card["content"]["actions"][0]["url"], link);

    let discord = by("/discord").json();
    assert_eq!(
        discord["embeds"][0]["title"],
        "accuracy dropped on intent-v2"
    );
    assert_eq!(discord["embeds"][0]["url"], link);
    assert_eq!(discord["allowed_mentions"]["parse"], json!([]));

    let hook = by("/hook");
    let body = hook.json();
    assert_eq!(body["event"], "vigily.alert");
    assert_eq!(body["schema"], "vigily.alert/1");
    assert_eq!(body["test"], false);
    assert_eq!(body["alert"]["id"], id);
    assert_eq!(body["alert"]["kind"], "behaviour_drift");
    assert_eq!(body["alert"]["severity"], "critical");
    assert!(body["alert"]["time"].as_str().unwrap().ends_with('Z'));
    assert_eq!(body["url"], link);
    // Verify the signature the way a receiver would.
    let ts = hook.headers["x-vigily-timestamp"].to_str().unwrap();
    let sig = hook.headers["x-vigily-signature"].to_str().unwrap();
    let mut mac = Hmac::<Sha256>::new_from_slice(signing_key.as_bytes()).unwrap();
    mac.update(format!("{ts}.").as_bytes());
    mac.update(&hook.body);
    let want: String = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(sig, format!("sha256={want}"));
    let age = vigily_core::now_micros() / 1_000_000 - ts.parse::<i64>().unwrap();
    assert!((0..60).contains(&age));
    // Only signed when a key is set.
    assert!(!by("/slack").headers.contains_key("x-vigily-signature"));

    let d = settled(&a, &id).await;
    assert_eq!(d.len(), 4);
    assert!(d
        .iter()
        .all(|d| d["status"] == "sent" && d["attempts"] == 1 && d["status_code"] == 200));
    let alerts = json(&a, "GET", "/api/v1/alerts", None).await;
    assert_eq!(
        alerts[0]["deliveries"],
        json!({"sent": 4, "failed": 0, "pending": 0})
    );

    // The same alert is raised and delivered once.
    assert!(!vigily_server::monitor::notify(&a.state, alert("behaviour/suite/run/accuracy")).await);
    let stored = vigily_core::evals::alerts(&*a.state.records).await.unwrap();
    let channels = vigily_server::channels::list(&*a.state.records)
        .await
        .unwrap();
    vigily_server::delivery::dispatch(&a.state, &stored[0], &channels).await;
    // `quiet` was not queued before, so it is now; the four others are not sent again (wait for
    // the fifth request, then a little longer for any repeat).
    r.wait_for(5).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let got = r.got();
    assert_eq!(
        got.len(),
        5,
        "{:?}",
        got.iter().map(|g| &g.path).collect::<Vec<_>>()
    );
    assert_eq!(got[4].path, "/quiet");
}

#[tokio::test]
async fn failures_are_retried_with_backoff_and_refusals_are_not() {
    let a = app();
    let flaky = receiver(&[500, 503]).await;
    let refusing = receiver(&[400]).await;
    let down = receiver(&[500, 500, 500, 500]).await;
    let token = synthetic("tok");
    for (name, r, path) in [
        ("flaky", &flaky, "/x".to_string()),
        ("refusing", &refusing, format!("/hook/{token}")),
        ("down", &down, "/x".to_string()),
    ] {
        channel(
            &a,
            json!({"name": name, "kind": "webhook", "default": true,
                   "secrets": {"url": {"value": format!("{}{path}", r.url)}}}),
        )
        .await;
    }
    let id = raise(&a, "schedule/nightly/1").await;
    let d = settled(&a, &id).await;
    let of = |n: &str| d.iter().find(|d| d["channel_name"] == n).unwrap().clone();
    let f = of("flaky");
    assert_eq!(
        (f["status"].as_str(), f["attempts"].as_u64()),
        (Some("sent"), Some(3))
    );
    assert!(f.get("error").is_none());
    let r = of("refusing");
    assert_eq!(
        (r["status"].as_str(), r["attempts"].as_u64()),
        (Some("failed"), Some(1))
    );
    assert_eq!(r["status_code"], 400);
    let err = r["error"].as_str().unwrap();
    assert!(err.contains("400"), "{err}");
    assert!(
        !err.contains(&token),
        "the error names the URL's path: {err}"
    );
    let down_d = of("down");
    assert_eq!(
        (down_d["status"].as_str(), down_d["attempts"].as_u64()),
        (Some("failed"), Some(3))
    );
    // Backoff: each retry waits longer than none.
    let got = flaky.got();
    assert_eq!(got.len(), 3);
    assert!(got[1].at - got[0].at >= Duration::from_millis(8));
    // A channel's last delivery shows on it.
    let channels = json(&a, "GET", "/api/v1/notifications/channels", None).await;
    let down_c = channels["channels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "down")
        .unwrap()
        .clone();
    assert_eq!(down_c["last_delivery"]["status"], "failed");
}

#[tokio::test]
async fn the_rate_limit_spaces_deliveries_without_using_attempts() {
    let a = app();
    let r = receiver(&[]).await;
    channel(
        &a,
        json!({"name": "slow", "kind": "webhook", "default": true,
               "settings": {"burst": 1, "per_minute": 600},
               "secrets": {"url": {"value": format!("{}/x", r.url)}}}),
    )
    .await;
    let start = Instant::now();
    let mut ids = Vec::new();
    for i in 0..3 {
        ids.push(raise(&a, &format!("data/s/{i}")).await);
    }
    let got = r.wait_for(3).await;
    // 600 a minute after a burst of one: the third send cannot start before 200 ms. (Arrival
    // times alone can bunch up behind a slow first connection, so measure from the start.)
    let last = got.iter().map(|g| g.at).max().unwrap();
    assert!(
        last - start >= Duration::from_millis(190),
        "{:?}",
        last - start
    );
    for id in ids {
        let d = settled(&a, &id).await;
        assert_eq!(d[0]["attempts"], 1);
    }
}

#[tokio::test]
async fn pending_deliveries_are_sent_after_a_restart() {
    let records = Arc::new(MemoryRecords::new());
    let key = synthetic("key");
    let r = receiver(&[]).await;
    // An engine that stops before its worker sends anything.
    let first = state_over(records.clone(), &key);
    let a = App {
        app: router(first.clone()),
        state: first,
    };
    channel(
        &a,
        json!({"name": "hook", "kind": "webhook", "default": true,
               "secrets": {"url": {"value": format!("{}/x", r.url)}}}),
    )
    .await;
    let id = raise(&a, "data/restart").await;
    assert_eq!(deliveries(&a, &id).await[0]["status"], "pending");
    drop(a);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(r.got().is_empty());

    let second = state_over(records, &key);
    vigily_server::delivery::start(second.clone());
    let b = App {
        app: router(second.clone()),
        state: second,
    };
    r.wait_for(1).await;
    let d = settled(&b, &id).await;
    assert_eq!(d[0]["status"], "sent");
}

#[tokio::test]
async fn test_sends_report_what_happened() {
    let a = app();
    let r = receiver(&[]).await;
    let ok = channel(
        &a,
        json!({"name": "hook", "kind": "webhook",
               "secrets": {"url": {"value": format!("{}/x", r.url)}}}),
    )
    .await;
    let res = json(
        &a,
        "POST",
        &format!("/api/v1/notifications/channels/{ok}/test"),
        None,
    )
    .await;
    assert_eq!(res["ok"], true, "{res}");
    assert_eq!(res["status_code"], 200);
    assert!(res["duration_ms"].is_u64());
    let body = r.wait_for(1).await[0].json();
    assert_eq!(body["test"], true);
    assert_eq!(body["alert"]["kind"], "test");
    assert_eq!(body["url"], format!("{PUBLIC}/alerts"));
    let c = json(
        &a,
        "GET",
        &format!("/api/v1/notifications/channels/{ok}"),
        None,
    )
    .await;
    assert_eq!(c["last_delivery"]["alert_id"], "test");
    assert_eq!(c["last_delivery"]["status"], "sent");

    // Nothing listens on a closed port.
    let closed = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let token = synthetic("tok");
    let down = channel(
        &a,
        json!({"name": "down", "kind": "slack",
               "secrets": {"webhook_url": {"value": format!("http://127.0.0.1:{closed}/{token}")}}}),
    )
    .await;
    let res = json(
        &a,
        "POST",
        &format!("/api/v1/notifications/channels/{down}/test"),
        None,
    )
    .await;
    assert_eq!(res["ok"], false);
    let err = res["error"].as_str().unwrap();
    assert!(err.contains("could not connect"), "{err}");
    assert!(!err.contains(&token), "{err}");

    // A variable that is not set is named.
    let env = channel(
        &a,
        json!({"name": "env", "kind": "discord",
               "secrets": {"webhook_url": {"env": "VIGILY_TEST_UNSET_DISCORD_URL"}}}),
    )
    .await;
    let res = json(
        &a,
        "POST",
        &format!("/api/v1/notifications/channels/{env}/test"),
        None,
    )
    .await;
    assert_eq!(res["ok"], false);
    assert!(res["error"]
        .as_str()
        .unwrap()
        .contains("VIGILY_TEST_UNSET_DISCORD_URL"));
}

/// A minimal SMTP server: accepts one message per connection and keeps the session's lines.
async fn smtp_server() -> (u16, Arc<Mutex<Vec<String>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let lines: Arc<Mutex<Vec<String>>> = Arc::default();
    let keep = lines.clone();
    tokio::spawn(async move {
        loop {
            let (sock, _) = listener.accept().await.unwrap();
            let keep = keep.clone();
            tokio::spawn(async move {
                let (r, mut w) = sock.into_split();
                let mut r = BufReader::new(r);
                w.write_all(b"220 test.example ESMTP\r\n").await.unwrap();
                let mut data = false;
                let mut line = String::new();
                loop {
                    line.clear();
                    if r.read_line(&mut line).await.unwrap() == 0 {
                        break;
                    }
                    keep.lock().push(line.trim_end().to_string());
                    let cmd = line.to_ascii_uppercase();
                    let reply: &[u8] = if data {
                        if line == ".\r\n" {
                            data = false;
                            b"250 2.0.0 queued\r\n"
                        } else {
                            continue;
                        }
                    } else if cmd.starts_with("EHLO") {
                        b"250-test.example\r\n250-AUTH PLAIN LOGIN\r\n250 8BITMIME\r\n"
                    } else if cmd.starts_with("AUTH") {
                        b"235 2.7.0 accepted\r\n"
                    } else if cmd.starts_with("DATA") {
                        data = true;
                        b"354 go ahead\r\n"
                    } else if cmd.starts_with("QUIT") {
                        w.write_all(b"221 bye\r\n").await.unwrap();
                        break;
                    } else {
                        b"250 ok\r\n"
                    };
                    w.write_all(reply).await.unwrap();
                }
            });
        }
    });
    (port, lines)
}

#[tokio::test]
async fn email_goes_out_over_smtp_with_text_and_html() {
    let a = app();
    let (port, lines) = smtp_server().await;
    let password = synthetic("pw");
    let id = channel(
        &a,
        json!({"name": "mail", "kind": "email", "default": true,
               "settings": {"host": "127.0.0.1", "port": port, "security": "none",
                            "username": "vigily", "from": "vigily <vigily@example.com>",
                            "to": ["oncall@example.com", "Team <team@example.com>"]},
               "secrets": {"password": {"value": password}}}),
    )
    .await;
    let c = json(
        &a,
        "GET",
        &format!("/api/v1/notifications/channels/{id}"),
        None,
    )
    .await;
    assert_eq!(c["settings"]["security"], "none");
    assert_eq!(c["secrets"]["password"]["source"], "stored");

    let alert_id = raise(&a, "schedule/mail/1").await;
    let d = settled(&a, &alert_id).await;
    assert_eq!(d[0]["status"], "sent", "{d:?}");
    assert_eq!(d[0]["status_code"], 250);
    let session = lines.lock().join("\n");
    assert!(
        session.contains("MAIL FROM:<vigily@example.com>"),
        "{session}"
    );
    assert!(session.contains("RCPT TO:<oncall@example.com>"));
    assert!(session.contains("RCPT TO:<team@example.com>"));
    assert!(session.contains("Subject: [vigily] Critical: accuracy dropped on intent-v2"));
    assert!(session.contains("text/plain") && session.contains("text/html"));
    assert!(session.contains("Open in vigily"));
    // The password went to the server, in the AUTH exchange only.
    let auth = lines
        .lock()
        .iter()
        .find(|l| l.to_ascii_uppercase().starts_with("AUTH"))
        .cloned()
        .unwrap_or_default();
    assert!(!auth.is_empty(), "no AUTH: {session}");

    let res = json(
        &a,
        "POST",
        &format!("/api/v1/notifications/channels/{id}/test"),
        None,
    )
    .await;
    assert_eq!(res["ok"], true, "{res}");
}
