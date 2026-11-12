//! Authentication: the local token (bearer and cookie), the sign-in redirect, the Host check,
//! the Origin check for cookie writes, server mode with scoped and stored-hashed tokens, the
//! 401 and 403 answers, and OTLP ingest.

#![recursion_limit = "256"]

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;
use vigily_core::{MemoryRecords, MemoryStore, NewRun, RecordStore, RunStatus, Store};
use vigily_server::auth::AuthMode;
use vigily_server::config::Config;
use vigily_server::{router, AppState, EngineOptions};

const LOCAL: &str = "0123456789abcdef0123456789abcdef";
const ADMIN: &str = "admin-token-for-tests-only-0001";
const RUN: &str = "0190a0b0c0d07e0f8a9b0c1d2e3f4a5b";

struct App {
    app: axum::Router,
    state: Arc<AppState>,
    records: Arc<MemoryRecords>,
    run: String,
}

async fn app(mode: AuthMode) -> App {
    let store: Arc<dyn Store> = Arc::new(MemoryStore::new());
    let w = store
        .create_run(NewRun {
            name: Some("existing".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    w.finish(RunStatus::Finished, None).await.unwrap();
    let records = Arc::new(MemoryRecords::new());
    let state = AppState::with_options(
        store,
        records.clone(),
        Config::default(),
        EngineOptions {
            auth: mode,
            ..Default::default()
        },
    );
    state.catalog.refresh().await.unwrap();
    App {
        app: router(state.clone()),
        state,
        records,
        run: w.info().id,
    }
}

fn local() -> AuthMode {
    AuthMode::Local {
        token: LOCAL.into(),
    }
}

fn server() -> AuthMode {
    AuthMode::Server {
        admin_token: ADMIN.into(),
    }
}

async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, HeaderMap, Value) {
    let res = app.clone().oneshot(req).await.unwrap();
    let (status, headers) = (res.status(), res.headers().clone());
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let v = serde_json::from_slice(&bytes)
        .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, headers, v)
}

/// A request to `uri` with `token` as a bearer token (none when empty).
fn req(method: &str, uri: &str, token: &str, body: Option<Value>) -> Request<Body> {
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "localhost");
    if !token.is_empty() {
        b = b.header("authorization", format!("Bearer {token}"));
    }
    match body {
        Some(v) => b
            .header("content-type", "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => b.body(Body::empty()).unwrap(),
    }
}

async fn status(
    app: &axum::Router,
    method: &str,
    uri: &str,
    token: &str,
    body: Option<Value>,
) -> StatusCode {
    send(app, req(method, uri, token, body)).await.0
}

#[tokio::test]
async fn the_local_token_opens_the_api_and_nothing_else_does() {
    let a = app(local()).await;
    let (st, h, v) = send(&a.app, req("GET", "/api/v1/runs", "", None)).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    assert_eq!(v["error"]["code"], "unauthorized");
    assert!(v["error"]["message"]
        .as_str()
        .unwrap()
        .contains("vigily up"));
    assert_eq!(h["www-authenticate"], "Bearer realm=\"vigily\"");
    let (st, _, v) = send(&a.app, req("GET", "/api/v1/runs", "wrong", None)).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    assert!(v["error"]["message"].as_str().unwrap().contains("unknown"));
    assert_eq!(
        status(&a.app, "GET", "/api/v1/runs", LOCAL, None).await,
        StatusCode::OK
    );
    // The local token may do everything.
    assert_eq!(
        status(
            &a.app,
            "POST",
            "/api/v1/admin/optimize",
            LOCAL,
            Some(json!({}))
        )
        .await,
        StatusCode::OK
    );
    // The legacy aliases need it too, and keep their flat error body.
    let (st, _, v) = send(&a.app, req("GET", "/api/runs", "", None)).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    assert!(v["error"].is_string(), "{v}");
    assert_eq!(
        status(&a.app, "GET", "/api/runs", LOCAL, None).await,
        StatusCode::OK
    );
}

#[tokio::test]
async fn health_info_me_and_the_web_app_need_no_token() {
    let a = app(local()).await;
    assert_eq!(
        status(&a.app, "GET", "/api/v1/health", "", None).await,
        StatusCode::OK
    );
    assert_eq!(
        status(&a.app, "GET", "/api/health", "", None).await,
        StatusCode::OK
    );
    let (st, _, v) = send(&a.app, req("GET", "/api/v1/info", "", None)).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(v["mode"], "local");
    let (_, _, v) = send(&a.app, req("GET", "/api/v1/auth/me", "", None)).await;
    assert_eq!(v["authenticated"], false);
    assert_eq!(v["auth_required"], true);
    let (_, _, v) = send(&a.app, req("GET", "/api/v1/auth/me", LOCAL, None)).await;
    assert_eq!(v["authenticated"], true);
    assert_eq!(v["principal"], "local");
    assert_ne!(
        status(&a.app, "GET", "/runs", "", None).await,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn the_token_link_sets_the_cookie_and_redirects() {
    let a = app(local()).await;
    let (st, h, _) = send(
        &a.app,
        req("GET", &format!("/runs?x=1&token={LOCAL}"), "", None),
    )
    .await;
    assert_eq!(st, StatusCode::SEE_OTHER);
    assert_eq!(h["location"], "/runs?x=1");
    let cookie = h["set-cookie"].to_str().unwrap();
    assert!(
        cookie.starts_with(&format!("vigily_token={LOCAL};")),
        "{cookie}"
    );
    for part in ["HttpOnly", "SameSite=Strict", "Path=/"] {
        assert!(cookie.contains(part), "{cookie}");
    }
    assert!(!cookie.contains("Secure"));
    let (st, h, _) = send(&a.app, req("GET", &format!("/?token={LOCAL}"), "", None)).await;
    assert_eq!(st, StatusCode::SEE_OTHER);
    assert_eq!(h["location"], "/");
    // A wrong token is not traded for a cookie.
    let (st, h, _) = send(&a.app, req("GET", "/?token=wrong", "", None)).await;
    assert_ne!(st, StatusCode::SEE_OTHER);
    assert!(h.get("set-cookie").is_none());
    // Reads with the cookie.
    let r = Request::get("/api/v1/runs")
        .header("host", "localhost")
        .header("cookie", format!("theme=dark; vigily_token={LOCAL}"))
        .body(Body::empty())
        .unwrap();
    assert_eq!(send(&a.app, r).await.0, StatusCode::OK);
}

fn cookie_patch(run: &str, origin: Option<&str>) -> Request<Body> {
    let mut b = Request::patch(format!("/api/v1/runs/{run}"))
        .header("host", "localhost:7600")
        .header("content-type", "application/json")
        .header("cookie", format!("vigily_token={LOCAL}"));
    if let Some(o) = origin {
        b = b.header("origin", o);
    }
    b.body(Body::from(json!({"name": "renamed"}).to_string()))
        .unwrap()
}

#[tokio::test]
async fn cookie_writes_must_come_from_the_engines_own_origin() {
    let a = app(local()).await;
    let (st, _, v) = send(&a.app, cookie_patch(&a.run, Some("http://evil.example"))).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(v["error"]["code"], "forbidden");
    assert_eq!(
        send(&a.app, cookie_patch(&a.run, None)).await.0,
        StatusCode::FORBIDDEN
    );
    let (st, _, v) = send(&a.app, cookie_patch(&a.run, Some("http://localhost:7600"))).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["name"], "renamed");
    // A bearer token needs no Origin.
    let r = req(
        "PATCH",
        &format!("/api/v1/runs/{}", a.run),
        LOCAL,
        Some(json!({"name": "again"})),
    );
    assert_eq!(send(&a.app, r).await.0, StatusCode::OK);
}

async fn create_token(a: &App, name: &str, scopes: Value) -> String {
    let (st, _, v) = send(
        &a.app,
        req(
            "POST",
            "/api/v1/auth/tokens",
            ADMIN,
            Some(json!({"name": name, "scopes": scopes})),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{v}");
    assert_eq!(v["name"], name);
    assert_eq!(v["created_by"], "admin");
    v["token"].as_str().unwrap().to_string()
}

fn batch() -> Value {
    json!({"writer": "w", "seq": 1, "ops": [{"op": "create", "id": RUN}]})
}

#[tokio::test]
async fn server_mode_tokens_carry_scopes() {
    let a = app(server()).await;
    let (_, _, info) = send(&a.app, req("GET", "/api/v1/info", "", None)).await;
    assert_eq!(info["mode"], "server");
    assert_eq!(
        status(&a.app, "GET", "/api/v1/runs", "", None).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        status(&a.app, "GET", "/api/v1/runs", ADMIN, None).await,
        StatusCode::OK
    );
    // The local token of another mode means nothing here.
    assert_eq!(
        status(&a.app, "GET", "/api/v1/runs", LOCAL, None).await,
        StatusCode::UNAUTHORIZED
    );

    let read = create_token(&a, "viewer", json!(["read"])).await;
    let ingest = create_token(&a, "ci", json!(["ingest"])).await;
    let admin = create_token(&a, "operator", json!(["admin"])).await;
    let both = create_token(&a, "sdk", json!(["read", "ingest"])).await;

    assert_eq!(
        status(&a.app, "GET", "/api/v1/runs", &read, None).await,
        StatusCode::OK
    );
    let (st, _, v) = send(&a.app, req("POST", "/api/v1/ingest", &read, Some(batch()))).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    assert_eq!(v["error"]["code"], "forbidden");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("ingest scope"),
        "{v}"
    );
    assert_eq!(
        status(
            &a.app,
            "POST",
            "/api/v1/admin/optimize",
            &read,
            Some(json!({}))
        )
        .await,
        StatusCode::FORBIDDEN
    );

    assert_eq!(
        status(&a.app, "POST", "/api/v1/ingest", &ingest, Some(batch())).await,
        StatusCode::OK
    );
    assert_eq!(
        status(&a.app, "GET", "/api/v1/runs", &ingest, None).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        status(
            &a.app,
            "POST",
            "/api/v1/runs/delete",
            &ingest,
            Some(json!({"ids": [a.run]}))
        )
        .await,
        StatusCode::FORBIDDEN
    );

    assert_eq!(
        status(&a.app, "GET", "/api/v1/runs", &both, None).await,
        StatusCode::OK
    );
    assert_eq!(
        status(&a.app, "GET", "/api/v1/auth/tokens", &both, None).await,
        StatusCode::FORBIDDEN
    );

    // Admin includes the other scopes.
    for (m, u, body) in [
        ("GET", "/api/v1/runs", None),
        ("POST", "/api/v1/flush", Some(json!({}))),
        ("POST", "/api/v1/admin/optimize", Some(json!({}))),
        ("GET", "/api/v1/auth/tokens", None),
    ] {
        assert_eq!(
            status(&a.app, m, u, &admin, body).await,
            StatusCode::OK,
            "{m} {u}"
        );
    }
    let (_, _, me) = send(&a.app, req("GET", "/api/v1/auth/me", &ingest, None)).await;
    assert_eq!(me["principal"], "token:ci");
    assert_eq!(me["scopes"], json!(["ingest"]));
}

#[tokio::test]
async fn tokens_are_stored_hashed_and_can_be_revoked() {
    let a = app(server()).await;
    let token = create_token(&a, "ci", json!(["read", "ingest"])).await;
    assert!(token.starts_with("vgy_"));
    let rows = a.records.scan("auth/token/").await.unwrap();
    assert_eq!(rows.len(), 1);
    let stored = rows[0].1.to_string();
    assert!(!stored.contains(&token), "the secret must not be stored");
    let secret = token.rsplit('_').next().unwrap();
    assert!(!stored.contains(secret));
    assert_eq!(rows[0].1["hash"], vigily_core::tokens::hash(&token));

    let (_, _, list) = send(&a.app, req("GET", "/api/v1/auth/tokens", ADMIN, None)).await;
    let listed = list["tokens"][0].clone();
    assert!(
        listed.get("token").is_none() && listed.get("hash").is_none(),
        "{listed}"
    );
    assert_eq!(
        status(&a.app, "GET", "/api/v1/runs", &token, None).await,
        StatusCode::OK
    );
    // Its first use is recorded.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let (_, _, list) = send(&a.app, req("GET", "/api/v1/auth/tokens", ADMIN, None)).await;
    assert!(list["tokens"][0]["last_used_at"].is_i64(), "{list}");

    let id = listed["id"].as_str().unwrap();
    let (st, _, v) = send(
        &a.app,
        req("DELETE", &format!("/api/v1/auth/tokens/{id}"), ADMIN, None),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let (st, _, v) = send(&a.app, req("GET", "/api/v1/runs", &token, None)).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    assert_eq!(v["error"]["code"], "unauthorized");
    assert_eq!(
        status(
            &a.app,
            "DELETE",
            &format!("/api/v1/auth/tokens/{id}"),
            ADMIN,
            None
        )
        .await,
        StatusCode::NOT_FOUND
    );
    // Expired tokens are refused.
    let (st, _, _) = send(
        &a.app,
        req(
            "POST",
            "/api/v1/auth/tokens",
            ADMIN,
            Some(json!({"name": "old", "scopes": ["read"], "expires_at": 1})),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn the_session_route_signs_a_browser_in() {
    let a = app(server()).await;
    let (st, _, _) = send(
        &a.app,
        req(
            "POST",
            "/api/v1/auth/session",
            "",
            Some(json!({"token": "nope"})),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let r = Request::post("/api/v1/auth/session")
        .header("host", "vigily.example")
        .header("x-forwarded-proto", "https")
        .header("content-type", "application/json")
        .body(Body::from(json!({"token": ADMIN}).to_string()))
        .unwrap();
    let (st, h, v) = send(&a.app, r).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["principal"], "admin");
    let cookie = h["set-cookie"].to_str().unwrap();
    assert!(
        cookie.contains("HttpOnly")
            && cookie.contains("SameSite=Strict")
            && cookie.contains("Secure")
    );
    let (_, h, _) = send(&a.app, req("DELETE", "/api/v1/auth/session", "", None)).await;
    assert!(h["set-cookie"].to_str().unwrap().contains("Max-Age=0"));
}

#[tokio::test]
async fn otlp_traces_need_a_token_with_ingest() {
    let a = app(server()).await;
    let body = json!({"resourceSpans": []});
    let (st, _, v) = send(&a.app, req("POST", "/v1/traces", "", Some(body.clone()))).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    assert_eq!(v["error"]["code"], "unauthorized");
    let read = create_token(&a, "viewer", json!(["read"])).await;
    assert_eq!(
        status(&a.app, "POST", "/v1/traces", &read, Some(body.clone())).await,
        StatusCode::FORBIDDEN
    );
    let ingest = create_token(&a, "otel", json!(["ingest"])).await;
    assert_eq!(
        status(&a.app, "POST", "/v1/traces", &ingest, Some(body.clone())).await,
        StatusCode::OK
    );
    assert_eq!(
        status(&a.app, "POST", "/v1/traces", ADMIN, Some(body)).await,
        StatusCode::OK
    );
}

#[tokio::test]
async fn short_lived_tokens_expire_and_are_revoked() {
    let a = app(local()).await;
    let t = a.state.auth.issue_ephemeral(
        "schedule:nightly",
        &[
            vigily_api::auth::Scope::Read,
            vigily_api::auth::Scope::Ingest,
        ],
        Duration::from_secs(60),
    );
    let (_, _, me) = send(&a.app, req("GET", "/api/v1/auth/me", &t, None)).await;
    assert_eq!(me["principal"], "schedule:nightly");
    assert_eq!(
        status(&a.app, "GET", "/api/v1/runs", &t, None).await,
        StatusCode::OK
    );
    assert_eq!(
        status(&a.app, "GET", "/api/v1/auth/tokens", &t, None).await,
        StatusCode::FORBIDDEN
    );
    a.state.auth.revoke_ephemeral(&t);
    assert_eq!(
        status(&a.app, "GET", "/api/v1/runs", &t, None).await,
        StatusCode::UNAUTHORIZED
    );
    let gone = a
        .state
        .auth
        .issue_ephemeral("x", &[vigily_api::auth::Scope::Read], Duration::ZERO);
    assert_eq!(
        status(&a.app, "GET", "/api/v1/runs", &gone, None).await,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn without_auth_every_request_is_allowed() {
    let a = app(AuthMode::default()).await;
    assert_eq!(
        status(&a.app, "GET", "/api/v1/runs", "", None).await,
        StatusCode::OK
    );
    let (_, _, me) = send(&a.app, req("GET", "/api/v1/auth/me", "", None)).await;
    assert_eq!(me["auth_required"], false);
    assert_eq!(me["principal"], "anonymous");
}

/// The served engine: the token file, the sign-in link and the Host check.
#[tokio::test(flavor = "multi_thread")]
async fn serve_keeps_the_local_token_in_a_private_file_and_checks_the_host() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("state").join("repo.token");
    let state = AppState::new(Arc::new(MemoryStore::new()));
    let (tx, rx) = tokio::sync::oneshot::channel();
    let opts = vigily_server::ServeOptions {
        addr: "127.0.0.1:0".parse().unwrap(),
        local_only: true,
        optimize: false,
        optimize_every: Duration::from_secs(3600),
        optimize_at_start: false,
        auth: vigily_server::AuthSetup::Local {
            token_file: file.clone(),
        },
    };
    let task = tokio::spawn(vigily_server::serve(state, opts, move |r| {
        let _ = tx.send(r.clone());
    }));
    let ready = rx.await.unwrap();
    let token = std::fs::read_to_string(&file).unwrap().trim().to_string();
    assert_eq!(token.len(), 32);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
    assert_eq!(
        ready.sign_in_url.as_deref(),
        Some(format!("{}/?token={token}", ready.url).as_str())
    );
    let http = reqwest::Client::new();
    let url = format!("{}/api/v1/runs", ready.url);
    let ok = http.get(&url).bearer_auth(&token).send().await.unwrap();
    assert_eq!(ok.status(), 200);
    let rebound = http
        .get(&url)
        .bearer_auth(&token)
        .header("host", "evil.example")
        .send()
        .await
        .unwrap();
    assert_eq!(rebound.status(), 403);
    task.abort();
    // The same file gives the same token next time.
    assert_eq!(
        vigily_server::auth::load_or_create_token(&file).unwrap(),
        token
    );
}
