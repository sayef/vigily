//! Who may call the API: tokens, the browser session cookie and the scope each route needs.
//!
//! Every request passes [`guard`], which finds the caller from an `Authorization: Bearer` header
//! or the `vigily_token` cookie and attaches an [`AuthContext`]. Routes are grouped by the scope
//! they need ([`Scoped`]); each group checks it with [`require`]. `GET /api/v1/health`, `info`,
//! `auth/me`, `auth/session` and the web app's files need no token.
//!
//! | Method and path                  | What it does |
//! |----------------------------------|--------------|
//! | `GET /api/v1/auth/me`            | Who the request is, its scopes and the engine's mode |
//! | `POST /api/v1/auth/session`      | `{token}`: sets the session cookie for the web app |
//! | `DELETE /api/v1/auth/session`    | Clears the session cookie |
//! | `GET /api/v1/auth/tokens`        | Stored access tokens (admin) |
//! | `POST /api/v1/auth/tokens`       | `{name, scopes, expires_at?}`: a new token, shown once (admin) |
//! | `DELETE /api/v1/auth/tokens/{id}`| Revokes a token (admin) |
//!
//! In local mode `GET /?token=...` (any page) sets the cookie and redirects to the same page
//! without the token. A request authenticated by the cookie that changes something must come from
//! vigily's own pages (`Origin` equal to the `Host`); a bearer token needs no such check.

use std::collections::HashMap;
use std::path::Path as FsPath;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::{Path, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Extension, Json, Router};
use parking_lot::{Mutex, RwLock};
use vigily_api::auth::{
    CreateToken, CreatedToken, Me, Revoked, RotatedToken, Scope, SessionRequest, Tokens,
};
use vigily_core::tokens::{self, TokenRecord};
use vigily_core::{now_micros, RecordStore};

use crate::{ApiError, AppState};

/// The session cookie.
pub const COOKIE: &str = "vigily_token";

/// The prefix of stored access tokens: `vgy_<id>_<secret>`.
const TOKEN_PREFIX: &str = "vgy_";

/// The shortest admin token accepted.
pub const MIN_ADMIN_TOKEN: usize = 16;

/// How long a stored token read from the records is trusted before it is read again.
const CACHE_FOR: Duration = Duration::from_secs(30);

/// `last_used_at` is written at most this often per token.
const LAST_USED_EVERY: i64 = 5 * 60 * 1_000_000;

/// How an engine checks requests.
#[derive(Clone, Debug)]
pub enum AuthMode {
    /// No checks: every request has every scope. For tests and embedding, and `--insecure-no-auth`
    /// (`server` says which mode the engine reports).
    Off { server: bool },
    /// One user on this machine: the repository's token opens everything.
    Local { token: String },
    /// A shared engine: the operator's admin token opens everything; stored tokens carry scopes.
    Server { admin_token: String },
}

impl Default for AuthMode {
    fn default() -> Self {
        AuthMode::Off { server: false }
    }
}

impl AuthMode {
    /// `local` or `server`, as `GET info` and `GET auth/me` report it.
    pub fn name(&self) -> &'static str {
        match self {
            AuthMode::Off { server: false } | AuthMode::Local { .. } => "local",
            AuthMode::Off { server: true } | AuthMode::Server { .. } => "server",
        }
    }

    pub fn required(&self) -> bool {
        !matches!(self, AuthMode::Off { .. })
    }
}

/// How a request proved who it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Via {
    Bearer,
    Cookie,
    /// The engine checks no tokens.
    Nothing,
}

/// Who made a request and what it may do. Handlers read it with `Extension<AuthContext>`; the
/// principal goes into audit records.
#[derive(Clone, Debug)]
pub struct AuthContext {
    /// `local`, `admin`, `token:<name>`, `schedule:<name>`, or `anonymous` without authentication.
    pub principal: String,
    pub scopes: Vec<Scope>,
    pub via: Via,
}

impl AuthContext {
    fn everything(principal: &str, via: Via) -> Self {
        Self {
            principal: principal.to_string(),
            scopes: Scope::ALL.to_vec(),
            via,
        }
    }

    /// Whether the caller has `scope` (`admin` has every scope).
    pub fn allows(&self, scope: Scope) -> bool {
        self.scopes.contains(&scope) || self.scopes.contains(&Scope::Admin)
    }
}

/// What [`guard`] found out about a request, for [`require`].
#[derive(Clone, Debug, Default)]
struct Caller {
    ctx: Option<AuthContext>,
    /// A bearer token was sent but is unknown, revoked or expired.
    bad_token: bool,
    /// The session cookie was sent with a write from another origin.
    foreign_origin: bool,
}

/// The authentication state of an engine.
pub struct Auth {
    mode: RwLock<AuthMode>,
    /// Short-lived tokens held in memory only (scheduled commands), by hash.
    ephemeral: Mutex<HashMap<String, (AuthContext, Instant)>>,
    /// Stored tokens by id, with when they were read.
    cache: Mutex<HashMap<String, (Instant, Option<TokenRecord>)>>,
}

impl Auth {
    pub fn new(mode: AuthMode) -> Self {
        Self {
            mode: RwLock::new(mode),
            ephemeral: Mutex::new(HashMap::new()),
            cache: Mutex::new(HashMap::new()),
        }
    }

    pub fn mode(&self) -> AuthMode {
        self.mode.read().clone()
    }

    pub fn set_mode(&self, mode: AuthMode) {
        *self.mode.write() = mode;
    }

    /// A token for one scheduled command: `scopes` until `ttl` has passed or it is revoked.
    /// Held in memory only.
    pub fn issue_ephemeral(&self, principal: &str, scopes: &[Scope], ttl: Duration) -> String {
        let token = format!("{TOKEN_PREFIX}tmp_{}", new_secret());
        let mut e = self.ephemeral.lock();
        let now = Instant::now();
        e.retain(|_, (_, until)| *until > now);
        e.insert(
            tokens::hash(&token),
            (
                AuthContext {
                    principal: principal.to_string(),
                    scopes: scopes.to_vec(),
                    via: Via::Bearer,
                },
                now + ttl,
            ),
        );
        token
    }

    pub fn revoke_ephemeral(&self, token: &str) {
        self.ephemeral.lock().remove(&tokens::hash(token));
    }

    fn forget(&self, id: &str) {
        self.cache.lock().remove(id);
    }

    /// The caller a token stands for, or `None` for an unknown, revoked or expired token.
    pub async fn check(&self, records: &Arc<dyn RecordStore>, token: &str) -> Option<AuthContext> {
        let token = token.trim();
        if token.is_empty() {
            return None;
        }
        match self.mode() {
            AuthMode::Off { .. } => {
                return Some(AuthContext::everything("anonymous", Via::Nothing))
            }
            AuthMode::Local { token: t } if same(token, &t) => {
                return Some(AuthContext::everything("local", Via::Bearer))
            }
            AuthMode::Server { admin_token } if same(token, &admin_token) => {
                return Some(AuthContext::everything("admin", Via::Bearer))
            }
            _ => {}
        }
        let hash = tokens::hash(token);
        if let Some((ctx, until)) = self.ephemeral.lock().get(&hash) {
            return (*until > Instant::now()).then(|| ctx.clone());
        }
        let id = stored_id(token)?;
        let cached = self
            .cache
            .lock()
            .get(id)
            .filter(|(at, _)| at.elapsed() < CACHE_FOR)
            .map(|(_, r)| r.clone());
        let rec = match cached {
            Some(r) => r,
            None => {
                let r = tokens::get(&**records, id).await.ok().flatten();
                self.cache
                    .lock()
                    .insert(id.to_string(), (Instant::now(), r.clone()));
                r
            }
        }?;
        let now = now_micros();
        if !same(&hash, &rec.hash) || rec.expired(now) {
            return None;
        }
        if rec.last_used_at.is_none_or(|t| now - t > LAST_USED_EVERY) {
            let (rs, id, mut rec) = (records.clone(), id.to_string(), rec.clone());
            rec.last_used_at = Some(now);
            self.cache
                .lock()
                .insert(id.clone(), (Instant::now(), Some(rec.clone())));
            tokio::spawn(async move {
                if let Err(e) = tokens::put(&*rs, &id, &rec).await {
                    tracing::warn!(error = %e, "recording a token's last use failed");
                }
            });
        }
        Some(AuthContext {
            principal: format!("token:{}", rec.name),
            scopes: rec.scopes,
            via: Via::Bearer,
        })
    }
}

/// Compares two secrets without stopping at the first difference.
fn same(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// The id part of a stored token `vgy_<id>_<secret>`.
fn stored_id(token: &str) -> Option<&str> {
    let rest = token.strip_prefix(TOKEN_PREFIX)?;
    let (id, secret) = rest.split_once('_')?;
    (!id.is_empty() && !secret.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric()))
        .then_some(id)
}

/// 128 random bits as 32 hex digits.
pub fn new_secret() -> String {
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).expect("the operating system's random number generator");
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The repository's local token from `path`, created (mode 0600, directories 0700) when missing.
pub fn load_or_create_token(path: &FsPath) -> std::io::Result<String> {
    match std::fs::read_to_string(path) {
        Ok(t) if !t.trim().is_empty() => return Ok(t.trim().to_string()),
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        let mut b = std::fs::DirBuilder::new();
        b.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut b, 0o700);
        b.create(dir)?;
    }
    let token = new_secret();
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
    std::io::Write::write_all(&mut o.open(path)?, format!("{token}\n").as_bytes())?;
    Ok(token)
}

/// The admin token from `VIGILY_ADMIN_TOKEN`, or the file `VIGILY_ADMIN_TOKEN_FILE` names.
/// `Ok(None)` when neither is set.
pub fn admin_token_from_env() -> Result<Option<String>, String> {
    let token = match (
        std::env::var("VIGILY_ADMIN_TOKEN")
            .ok()
            .filter(|t| !t.is_empty()),
        std::env::var("VIGILY_ADMIN_TOKEN_FILE")
            .ok()
            .filter(|t| !t.is_empty()),
    ) {
        (Some(t), _) => t,
        (None, Some(file)) => std::fs::read_to_string(&file)
            .map_err(|e| format!("could not read VIGILY_ADMIN_TOKEN_FILE {file}: {e}"))?,
        (None, None) => return Ok(None),
    };
    let token = token.trim().to_string();
    if token.len() < MIN_ADMIN_TOKEN {
        return Err(format!(
            "the admin token is shorter than {MIN_ADMIN_TOKEN} characters; use a long random value, such as `openssl rand -hex 32`"
        ));
    }
    Ok(Some(token))
}

// ---------------------------------------------------------------------------------------------
// Middleware

fn cookie_value(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|c| c.trim().split_once('='))
        .find(|(k, _)| *k == COOKIE)
        .map(|(_, v)| v.to_string())
}

fn bearer(headers: &HeaderMap) -> Option<String> {
    let v = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = v.split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then(|| token.trim().to_string())
}

/// Whether a write sent with the session cookie comes from this engine's own pages.
fn same_origin(headers: &HeaderMap) -> bool {
    let h = |n: header::HeaderName| headers.get(n).and_then(|v| v.to_str().ok());
    match h(header::ORIGIN) {
        Some(origin) => {
            let host = h(header::HOST).unwrap_or("");
            !host.is_empty()
                && (origin == format!("http://{host}") || origin == format!("https://{host}"))
        }
        None => headers
            .get("sec-fetch-site")
            .is_some_and(|v| v == "same-origin"),
    }
}

/// Whether the client reached the engine over HTTPS (directly or through a proxy that says so).
fn https(headers: &HeaderMap) -> bool {
    headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').next().unwrap_or("").trim() == "https")
}

/// The session cookie for `token`; an empty token clears it.
fn session_cookie(token: &str, headers: &HeaderMap) -> HeaderValue {
    let age = if token.is_empty() { 0 } else { 31_536_000 };
    let secure = if https(headers) { "; Secure" } else { "" };
    HeaderValue::from_str(&format!(
        "{COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={age}{secure}"
    ))
    .unwrap_or_else(|_| HeaderValue::from_static(""))
}

fn is_api(path: &str) -> bool {
    path.starts_with("/api/") || path == "/api" || path.starts_with("/v1/")
}

/// Finds out who a request is (see the module documentation). Never refuses a request itself:
/// [`require`] does, per route group.
pub async fn guard(State(s): State<Arc<AppState>>, mut req: Request, next: Next) -> Response {
    let mode = s.auth.mode();
    if !mode.required() {
        req.extensions_mut().insert(Caller {
            ctx: Some(AuthContext::everything("anonymous", Via::Nothing)),
            ..Default::default()
        });
        req.extensions_mut()
            .insert(AuthContext::everything("anonymous", Via::Nothing));
        return next.run(req).await;
    }
    let path = req.uri().path().to_string();
    // `vigily up` prints a link with the local token: trade it for the cookie.
    if matches!(mode, AuthMode::Local { .. }) && req.method() == Method::GET && !is_api(&path) {
        let query = req.uri().query().unwrap_or("").to_string();
        let token = query
            .split('&')
            .find_map(|kv| kv.strip_prefix("token="))
            .map(str::to_string);
        if let Some(token) = token {
            if s.auth.check(&s.records, &token).await.is_some() {
                let rest: Vec<&str> = query
                    .split('&')
                    .filter(|kv| !kv.starts_with("token=") && !kv.is_empty())
                    .collect();
                let to = if rest.is_empty() {
                    path.clone()
                } else {
                    format!("{path}?{}", rest.join("&"))
                };
                let mut res = StatusCode::SEE_OTHER.into_response();
                let h = res.headers_mut();
                if let Ok(v) = HeaderValue::from_str(&to) {
                    h.insert(header::LOCATION, v);
                }
                h.insert(header::SET_COOKIE, session_cookie(&token, req.headers()));
                h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
                return res;
            }
        }
    }
    let mut caller = Caller::default();
    if let Some(token) = bearer(req.headers()) {
        match s.auth.check(&s.records, &token).await {
            Some(ctx) => caller.ctx = Some(ctx),
            None => caller.bad_token = true,
        }
    } else if let Some(token) = cookie_value(req.headers()) {
        if let Some(mut ctx) = s.auth.check(&s.records, &token).await {
            ctx.via = Via::Cookie;
            let safe = matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);
            if safe || same_origin(req.headers()) {
                caller.ctx = Some(ctx);
            } else {
                caller.foreign_origin = true;
            }
        }
    }
    if let Some(ctx) = &caller.ctx {
        req.extensions_mut().insert(ctx.clone());
    }
    req.extensions_mut().insert(caller);
    next.run(req).await
}

/// Refuses a request whose caller lacks `scope`: 401 without a valid token, 403 with one that may
/// not do this or with the session cookie from another origin.
pub async fn require(State(scope): State<Scope>, req: Request, next: Next) -> Response {
    let caller = req
        .extensions()
        .get::<Caller>()
        .cloned()
        .unwrap_or_default();
    let refused = match &caller.ctx {
        Some(ctx) if ctx.allows(scope) => return next.run(req).await,
        Some(ctx) => ApiError::new(
            StatusCode::FORBIDDEN,
            format!(
                "{} may not do this: it needs the {} scope",
                ctx.principal,
                scope.as_str()
            ),
        ),
        None if caller.foreign_origin => ApiError::new(
            StatusCode::FORBIDDEN,
            "the session cookie is accepted for changes only from vigily's own pages; send the token as Authorization: Bearer instead",
        ),
        None if caller.bad_token => ApiError::new(
            StatusCode::UNAUTHORIZED,
            "the token is unknown, revoked or expired",
        ),
        None => ApiError::new(
            StatusCode::UNAUTHORIZED,
            "this engine needs a token: open the link `vigily up` printed, sign in on the web app, or send Authorization: Bearer <token>",
        ),
    };
    let mut res = refused.into_response();
    if res.status() == StatusCode::UNAUTHORIZED {
        res.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            HeaderValue::from_static("Bearer realm=\"vigily\""),
        );
    }
    res
}

/// Routes grouped by the scope they need.
pub struct Scoped {
    pub open: Router<Arc<AppState>>,
    pub read: Router<Arc<AppState>>,
    pub ingest: Router<Arc<AppState>>,
    pub admin: Router<Arc<AppState>>,
}

impl Default for Scoped {
    fn default() -> Self {
        Self {
            open: Router::new(),
            read: Router::new(),
            ingest: Router::new(),
            admin: Router::new(),
        }
    }
}

impl Scoped {
    pub fn open(mut self, r: Router<Arc<AppState>>) -> Self {
        self.open = self.open.merge(r);
        self
    }

    pub fn read(mut self, r: Router<Arc<AppState>>) -> Self {
        self.read = self.read.merge(r);
        self
    }

    pub fn ingest(mut self, r: Router<Arc<AppState>>) -> Self {
        self.ingest = self.ingest.merge(r);
        self
    }

    pub fn admin(mut self, r: Router<Arc<AppState>>) -> Self {
        self.admin = self.admin.merge(r);
        self
    }

    pub fn merge(self, o: Scoped) -> Self {
        self.open(o.open)
            .read(o.read)
            .ingest(o.ingest)
            .admin(o.admin)
    }

    /// One router, each group behind [`require`] with its scope.
    pub fn build(self) -> Router<Arc<AppState>> {
        let guarded = |r: Router<Arc<AppState>>, scope: Scope| {
            if r.has_routes() {
                r.route_layer(axum::middleware::from_fn_with_state(scope, require))
            } else {
                r
            }
        };
        self.open
            .merge(guarded(self.read, Scope::Read))
            .merge(guarded(self.ingest, Scope::Ingest))
            .merge(guarded(self.admin, Scope::Admin))
    }
}

// ---------------------------------------------------------------------------------------------
// Routes

/// The authentication routes, relative to the API root.
pub fn routes() -> Scoped {
    Scoped::default()
        .open(
            Router::new()
                .route("/auth/me", get(me))
                .route("/auth/session", post(sign_in).delete(sign_out)),
        )
        .admin(
            Router::new()
                .route("/auth/tokens", get(list_tokens).post(create_token))
                .route("/auth/tokens/{id}", delete(revoke_token))
                .route("/auth/local-token/rotate", post(rotate_local_token)),
        )
}

/// Replaces the local token: a new secret in the token file and the engine file, at once in
/// effect. Links, cookies and scripts with the old one stop working.
async fn rotate_local_token(
    State(s): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    headers: HeaderMap,
    bytes: Bytes,
) -> Result<Json<RotatedToken>, ApiError> {
    crate::from_a_client_as(&headers, "token rotations", &["application/json", ""])?;
    let _: Option<serde_json::Value> = crate::admin_api::body(&bytes)?;
    if !matches!(s.auth.mode(), AuthMode::Local { .. }) {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "only an engine in local mode has a local token; in server mode, revoke and create access tokens",
        ));
    }
    let token = new_secret();
    let file = s.local_token_file.read().clone();
    if let Some(file) = &file {
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
        o.open(file)
            .and_then(|mut f| std::io::Write::write_all(&mut f, format!("{token}\n").as_bytes()))
            .map_err(|e| {
                ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("could not write the local token to {}: {e}", file.display()),
                )
            })?;
    }
    s.auth.set_mode(AuthMode::Local {
        token: token.clone(),
    });
    let engine_file = s.engine_file.read().clone();
    if let Some(path) = engine_file {
        let updated = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str::<vigily_api::local::EngineFile>(&t).ok())
            .map(|mut f| {
                f.token = Some(token.clone());
                crate::engine::write_engine_file(&path, &f)
            });
        if let Some(Err(e)) = updated {
            tracing::warn!(file = %path.display(), error = %e, "could not update the engine file");
        }
    }
    tracing::info!(by = %ctx.principal, "local token rotated");
    Ok(Json(RotatedToken { token }))
}

fn me_of(s: &AppState, ctx: Option<&AuthContext>) -> Me {
    let mode = s.auth.mode();
    Me {
        authenticated: ctx.is_some(),
        principal: ctx.map(|c| c.principal.clone()),
        scopes: ctx.map(|c| c.scopes.clone()).unwrap_or_default(),
        mode: mode.name().to_string(),
        auth_required: mode.required(),
    }
}

async fn me(State(s): State<Arc<AppState>>, ctx: Option<Extension<AuthContext>>) -> Json<Me> {
    Json(me_of(&s, ctx.as_deref()))
}

async fn sign_in(
    State(s): State<Arc<AppState>>,
    headers: HeaderMap,
    bytes: Bytes,
) -> Result<Response, ApiError> {
    crate::from_a_client(&headers, "sign-in requests")?;
    let req: SessionRequest = serde_json::from_slice(&bytes).map_err(|e| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            format!("send {{\"token\": \"...\"}}: {e}"),
        )
    })?;
    let mut ctx = s.auth.check(&s.records, &req.token).await.ok_or_else(|| {
        ApiError::new(
            StatusCode::UNAUTHORIZED,
            "the token is unknown, revoked or expired",
        )
    })?;
    ctx.via = Via::Cookie;
    let mut res = Json(me_of(&s, Some(&ctx))).into_response();
    res.headers_mut().insert(
        header::SET_COOKIE,
        session_cookie(req.token.trim(), &headers),
    );
    tracing::info!(principal = %ctx.principal, "signed in to the web app");
    Ok(res)
}

async fn sign_out(State(s): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let mut res = Json(me_of(&s, None)).into_response();
    res.headers_mut()
        .insert(header::SET_COOKIE, session_cookie("", &headers));
    res
}

async fn list_tokens(State(s): State<Arc<AppState>>) -> Result<Json<Tokens>, ApiError> {
    let tokens = tokens::list(&*s.records)
        .await?
        .into_iter()
        .map(|(id, t)| t.info(&id))
        .collect();
    Ok(Json(Tokens { tokens }))
}

async fn create_token(
    State(s): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    headers: HeaderMap,
    bytes: Bytes,
) -> Result<(StatusCode, Json<CreatedToken>), ApiError> {
    crate::from_a_client(&headers, "token requests")?;
    let req: CreateToken = serde_json::from_slice(&bytes).map_err(|e| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            format!("send {{\"name\", \"scopes\": [\"read\"|\"ingest\"|\"admin\"], \"expires_at\"?}}: {e}"),
        )
    })?;
    let name = req.name.trim();
    if name.is_empty() || name.len() > 64 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "a token needs a name of 1 to 64 characters, such as ci or nightly-evals",
        ));
    }
    if req.scopes.is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "a token needs at least one scope: read, ingest or admin",
        ));
    }
    let now = now_micros();
    if req.expires_at.is_some_and(|e| e <= now) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "expires_at is in the past",
        ));
    }
    let id = new_secret()[..16].to_string();
    let token = format!("{TOKEN_PREFIX}{id}_{}", new_secret());
    let mut scopes = req.scopes.clone();
    scopes.sort();
    scopes.dedup();
    let rec = TokenRecord {
        hash: tokens::hash(&token),
        name: name.to_string(),
        scopes,
        created_at: now,
        last_used_at: None,
        expires_at: req.expires_at,
        created_by: Some(ctx.principal.clone()),
    };
    tokens::put(&*s.records, &id, &rec).await?;
    s.auth.forget(&id);
    tracing::info!(id, name, by = %ctx.principal, "token created");
    Ok((
        StatusCode::CREATED,
        Json(CreatedToken {
            token,
            info: rec.info(&id),
        }),
    ))
}

async fn revoke_token(
    State(s): State<Arc<AppState>>,
    Extension(ctx): Extension<AuthContext>,
    Path(id): Path<String>,
) -> Result<Json<Revoked>, ApiError> {
    let revoked = tokens::delete(&*s.records, &id).await?;
    s.auth.forget(&id);
    if !revoked {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            format!("no token with id {id}"),
        ));
    }
    tracing::info!(id, by = %ctx.principal, "token revoked");
    Ok(Json(Revoked { revoked }))
}
