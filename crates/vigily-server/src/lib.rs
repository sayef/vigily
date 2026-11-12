//! vigily's HTTP server: a JSON API under `/api/v1` over a [`Catalog`], and the embedded web app
//! for every other path.
//!
//! | Method and path           | What it returns |
//! |---------------------------|-----------------|
//! | `GET /api/v1/health`      | `ok` |
//! | `GET /api/v1/info`        | Version, API version, mode and capabilities |
//! | `GET /api/v1/meta`        | Repository, counts, experiments, metric keys and parameter keys |
//! | `GET /api/v1/runs`        | Runs matching `q`, sorted by `sort` (a column id), with parameters and metric statistics |
//! | `GET /api/v1/runs/{id}`   | One run with its parameter tree and series |
//! | `PATCH /api/v1/runs/{id}` | Changes name, experiment, notes, tags or archived |
//! | `POST /api/v1/runs/delete`| Deletes runs that are not running |
//! | `POST /api/v1/chart`      | One metric for many runs, aligned and downsampled for a chart |
//!
//! Evaluations are served by [`evals_api`], traces by [`traces_api`]. Errors answer
//! `{"error": {"code", "message", "position"?}}` ([`vigily_api::ErrorBody`]).
//!
//! Every route is also served at its old path under `/api/` (without `v1`), with a `Deprecation`
//! header, a `Link` to its successor and the flat `{"error": "..."}` body of earlier releases.
//! OpenTelemetry's `POST /v1/traces` stays at the root.

pub mod admin_api;
pub mod config;
pub mod evals_api;
pub mod monitor;
pub mod runs_api;
pub mod scheduler;
pub mod traces_api;
pub mod write_api;
pub mod writers;

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, Query as QueryParams, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use parking_lot::Mutex;
use serde::Deserialize;
use vigily_api::chart::{ChartGroup, ChartRequest, ChartResponse, ChartSeries};
use vigily_api::runs::{
    DeleteRuns, Deleted, Meta, MetricStat, RunDetail, RunFields, RunRow, RunSummary, RunsResponse,
};
use vigily_api::{code, ErrorBody, ErrorDetail, Info, LegacyErrorBody, API_VERSION};
use vigily_core::chart::{aggregate, align, x_values, Xy};
use vigily_core::{
    Catalog, Column, Error as CoreError, MemoryRecords, Points, Query, RecordStore, RunEntry,
    RunPatch, RunStatus, StepRange, Store,
};

/// How often the catalog is refreshed in the background.
pub const REFRESH_EVERY: Duration = Duration::from_secs(2);

/// The longest run filter accepted, in bytes.
pub const MAX_QUERY: usize = 4096;

/// How often storage is compacted in the background, when enabled.
pub const OPTIMIZE_EVERY: Duration = Duration::from_secs(15 * 60);

/// Shared server state.
pub struct AppState {
    pub catalog: Catalog,
    /// Datasets, eval results, labels, schedule runs, alerts.
    pub records: Arc<dyn RecordStore>,
    pub config: config::Config,
    /// The next fire time of each running schedule (UTC microseconds).
    pub schedule_status: parking_lot::RwLock<BTreeMap<String, i64>>,
    pub http: reqwest::Client,
    /// Drift histories by suite and score (or `data`), with the fingerprint they were computed for.
    pub(crate) drift_cache: Mutex<HashMap<String, (String, CachedValue)>>,
    cache: Mutex<PointCache>,
    /// Serialises trace ingests and deletions (a trace's summary is read, updated and written).
    pub(crate) ingest_lock: tokio::sync::Mutex<()>,
    /// Trace summaries with the trace table's version they were read at.
    pub(crate) traces_cache: Mutex<Option<TracesCache>>,
    /// The number of traces and when it was counted (UTC microseconds).
    pub(crate) traces_count: Mutex<Option<(i64, usize)>>,
    /// Open run writers, applied ingest batches and the ingest budget.
    pub writes: writers::Writes,
    /// Wakes the background catalog refresh early (after a write).
    pub(crate) refresh_now: tokio::sync::Notify,
    pub version: &'static str,
}

/// Options of an engine beyond its storage and configuration file.
#[derive(Clone, Debug, Default)]
pub struct EngineOptions {
    /// Body limit and ingest budget of the write routes.
    pub limits: writers::WriteLimits,
}

impl AppState {
    /// A server over `store` with in-memory records and no configuration (tests, embedding).
    pub fn new(store: Arc<dyn Store>) -> Arc<Self> {
        Self::with(
            store,
            Arc::new(MemoryRecords::new()),
            config::Config::default(),
        )
    }

    pub fn with(
        store: Arc<dyn Store>,
        records: Arc<dyn RecordStore>,
        config: config::Config,
    ) -> Arc<Self> {
        Self::with_options(store, records, config, EngineOptions::default())
    }

    pub fn with_options(
        store: Arc<dyn Store>,
        records: Arc<dyn RecordStore>,
        config: config::Config,
        opts: EngineOptions,
    ) -> Arc<Self> {
        Arc::new(Self {
            catalog: Catalog::new(store),
            records,
            config,
            schedule_status: parking_lot::RwLock::new(BTreeMap::new()),
            drift_cache: Mutex::new(HashMap::new()),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap_or_default(),
            cache: Mutex::new(PointCache::new(8_000_000)),
            ingest_lock: tokio::sync::Mutex::new(()),
            traces_cache: Mutex::new(None),
            traces_count: Mutex::new(None),
            writes: writers::Writes::new(opts.limits),
            refresh_now: tokio::sync::Notify::new(),
            version: env!("CARGO_PKG_VERSION"),
        })
    }
}

/// Trace summaries and the storage version they were read at.
pub(crate) type TracesCache = (Option<u64>, Arc<Vec<vigily_core::traces::TraceSummary>>);

/// A cached computation of any type.
pub(crate) type CachedValue = Arc<dyn std::any::Any + Send + Sync>;

/// Raw points of finished series, so zooming does not read storage again. Bounded by a total
/// point budget, evicting the oldest entries first.
struct PointCache {
    budget: usize,
    used: usize,
    map: HashMap<u64, (u64, Arc<Points>)>,
    order: VecDeque<u64>,
}

impl PointCache {
    fn new(budget: usize) -> Self {
        Self {
            budget,
            used: 0,
            map: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn get(&self, id: u64, count: u64) -> Option<Arc<Points>> {
        self.map
            .get(&id)
            .filter(|(c, _)| *c == count)
            .map(|(_, p)| p.clone())
    }

    fn put(&mut self, id: u64, count: u64, p: Arc<Points>) {
        if p.len() > self.budget / 4 {
            return;
        }
        if let Some((_, old)) = self.map.insert(id, (count, p.clone())) {
            self.used -= old.len();
        } else {
            self.order.push_back(id);
        }
        self.used += p.len();
        while self.used > self.budget {
            let Some(old) = self.order.pop_front() else {
                break;
            };
            if let Some((_, q)) = self.map.remove(&old) {
                self.used -= q.len();
            }
        }
    }
}

/// An API error: a status, a machine-readable code and a sentence that names the problem and the
/// way out. On `/api/v1` it answers `{"error": {"code", "message", "position"?}}`; the deprecated
/// `/api/*` aliases rewrite it to the flat `{"error": "...", "position"?}` they always had.
#[derive(Clone, Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
    position: Option<usize>,
}

/// The code an error of this status has unless a route names a more specific one.
fn code_of(status: StatusCode) -> &'static str {
    match status {
        StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED => code::NOT_FOUND,
        StatusCode::CONFLICT => code::CONFLICT,
        StatusCode::UNAUTHORIZED => code::UNAUTHORIZED,
        StatusCode::FORBIDDEN => code::FORBIDDEN,
        StatusCode::PAYLOAD_TOO_LARGE => code::TOO_LARGE,
        StatusCode::SERVICE_UNAVAILABLE | StatusCode::TOO_MANY_REQUESTS => code::OVERLOADED,
        s if s.is_server_error() => code::INTERNAL,
        _ => code::INVALID_ARGUMENT,
    }
}

impl ApiError {
    pub(crate) fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            code: code_of(status),
            message: message.into(),
            position: None,
        }
    }

    /// The same error with a more specific code than its status implies.
    pub(crate) fn with_code(mut self, code: &'static str) -> Self {
        self.code = code;
        self
    }

    pub fn status(&self) -> StatusCode {
        self.status
    }

    pub fn code(&self) -> &'static str {
        self.code
    }

    /// The flat body of the deprecated `/api/*` aliases.
    fn legacy_response(&self) -> Response {
        (
            self.status,
            Json(LegacyErrorBody {
                error: self.message.clone(),
                position: self.position,
            }),
        )
            .into_response()
    }
}

impl From<CoreError> for ApiError {
    fn from(e: CoreError) -> Self {
        match e {
            CoreError::NotFound(m) => ApiError::new(StatusCode::NOT_FOUND, m),
            CoreError::InvalidArgument(m) => ApiError::new(StatusCode::BAD_REQUEST, m),
            other => ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, other.to_string()),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = ErrorBody {
            error: ErrorDetail {
                code: self.code.to_string(),
                message: self.message.clone(),
                position: self.position,
            },
        };
        let mut res = (self.status, Json(body)).into_response();
        // The legacy aliases rebuild their flat body from this.
        res.extensions_mut().insert(self);
        res
    }
}

type ApiResult<T> = Result<Json<T>, ApiError>;

/// The JSON API of earlier releases, relative to its root: mounted at `/api/v1`, and at `/api` as
/// deprecated aliases.
fn api() -> Router<Arc<AppState>> {
    Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/meta", get(meta))
        .route("/runs", get(runs))
        .route("/runs/{id}", get(run).patch(patch_run))
        .route("/runs/delete", post(delete_runs))
        .route("/chart", post(chart))
        .merge(evals_api::routes())
        .merge(traces_api::routes())
}

/// Routes added with `/api/v1`; the deprecated `/api` aliases do not serve them.
fn v1_only(state: &Arc<AppState>) -> Router<Arc<AppState>> {
    Router::new()
        .route("/info", get(info))
        .merge(write_api::routes(state, state.writes.limits))
        .merge(runs_api::routes())
        .merge(evals_api::v1_routes())
        .merge(traces_api::v1_routes())
        .merge(admin_api::routes())
}

/// The route groups this engine serves (`GET /api/v1/info`).
pub const CAPABILITIES: &[&str] = &[
    "runs",
    "points",
    "charts",
    "datasets",
    "evals",
    "schedules",
    "suites",
    "alerts",
    "traces",
    "otlp",
    "admin",
    "ingest",
];

async fn info(State(s): State<Arc<AppState>>) -> Json<Info> {
    Json(Info {
        version: s.version.to_string(),
        api_version: API_VERSION.to_string(),
        mode: "local".to_string(),
        capabilities: CAPABILITIES.iter().map(|c| c.to_string()).collect(),
    })
}

/// The router: `/api/v1`, its deprecated `/api/*` aliases, OTLP at `/v1/traces`, and the embedded
/// web app for every other path.
pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .nest(
            "/api/v1",
            api()
                .merge(v1_only(&state))
                .layer(axum::middleware::from_fn(v1_errors)),
        )
        .nest("/api", api().layer(axum::middleware::from_fn(legacy)))
        .merge(traces_api::otlp_routes())
        .fallback(asset)
        .layer(tower_http::compression::CompressionLayer::new())
        .with_state(state)
}

/// The largest error body that is rewritten into the structured shape; extractor rejections are a
/// sentence long.
const MAX_REWRITTEN_ERROR: usize = 64 * 1024;

/// Gives every `/api/v1` error the structured shape, including the plain-text rejections of
/// axum's extractors (a malformed JSON body, a missing query parameter, a body over the limit).
async fn v1_errors(req: axum::extract::Request, next: axum::middleware::Next) -> Response {
    let res = next.run(req).await;
    let status = res.status();
    if !(status.is_client_error() || status.is_server_error())
        || res.extensions().get::<ApiError>().is_some()
    {
        return res;
    }
    let bytes = axum::body::to_bytes(res.into_body(), MAX_REWRITTEN_ERROR)
        .await
        .unwrap_or_default();
    let text = String::from_utf8_lossy(&bytes).trim().to_string();
    let message = if text.is_empty() {
        status
            .canonical_reason()
            .unwrap_or("request failed")
            .to_lowercase()
    } else {
        text
    };
    ApiError::new(status, message).into_response()
}

/// The deprecated `/api/*` aliases: the same handlers, the flat error body they always had, and
/// headers that point to the `/api/v1` successor.
async fn legacy(req: axum::extract::Request, next: axum::middleware::Next) -> Response {
    // Nested under `/api`, the handler sees the path without that prefix.
    let successor = format!(
        "/api/v1{}{}",
        req.uri().path(),
        req.uri()
            .query()
            .map(|q| format!("?{q}"))
            .unwrap_or_default()
    );
    let res = next.run(req).await;
    let mut res = match res.extensions().get::<ApiError>() {
        Some(e) => e.legacy_response(),
        None => res,
    };
    let h = res.headers_mut();
    h.insert("deprecation", HeaderValue::from_static("true"));
    if let Ok(v) = HeaderValue::from_str(&format!("<{successor}>; rel=\"successor-version\"")) {
        h.insert(header::LINK, v);
    }
    res
}

/// Options for [`serve`].
pub struct ServeOptions {
    pub addr: SocketAddr,
    /// Answer only requests addressed to a loopback host name, against DNS rebinding.
    pub local_only: bool,
    /// Optimize storage (sort points by series, index them) every `optimize_every`, in the
    /// background.
    pub optimize: bool,
    /// How often; [`OPTIMIZE_EVERY`] by default.
    pub optimize_every: Duration,
    /// Optimize once before the first catalog refresh as well.
    pub optimize_at_start: bool,
}

async fn optimize(state: &AppState) {
    let started = std::time::Instant::now();
    match state.catalog.store().optimize().await {
        Ok(()) => tracing::info!(
            seconds = started.elapsed().as_secs_f64(),
            "storage compacted"
        ),
        Err(e) => tracing::warn!(error = %e, "storage compaction failed"),
    }
    // On a large heap trimming can take tens to hundreds of milliseconds: keep it off the
    // request-serving threads.
    if let Err(e) = tokio::task::spawn_blocking(release_memory).await {
        tracing::warn!(error = %e, "returning memory to the system failed");
    }
}

/// Hands memory freed by an optimize back to the system. glibc's allocator keeps it mapped
/// otherwise, and the server would stay at the optimize's peak (docs/architecture.md, "Memory").
fn release_memory() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: malloc_trim only returns free heap memory to the system; it takes no pointers.
    unsafe {
        libc::malloc_trim(0);
    }
}

/// Serves until Ctrl-C. Refreshes the catalog before listening and then in the background.
pub async fn serve(
    state: Arc<AppState>,
    opts: ServeOptions,
    ready: impl FnOnce(SocketAddr),
) -> std::io::Result<()> {
    if opts.optimize_at_start {
        optimize(&state).await;
    }
    if opts.optimize {
        let (bg, every) = (state.clone(), opts.optimize_every);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(every).await;
                optimize(&bg).await;
            }
        });
    }
    if let Err(e) = state.catalog.refresh().await {
        tracing::warn!(error = %e, "initial catalog refresh failed");
    }
    scheduler::start(state.clone());
    monitor::start(state.clone());
    let bg = state.clone();
    tokio::spawn(async move {
        loop {
            // A write wakes the refresh early, so new runs and points show up at once.
            tokio::select! {
                _ = tokio::time::sleep(REFRESH_EVERY) => {}
                _ = bg.refresh_now.notified() => {}
            }
            if let Err(e) = bg.catalog.refresh().await {
                tracing::warn!(error = %e, "catalog refresh failed");
            }
        }
    });
    let mut app = router(state);
    if opts.local_only {
        app = app.layer(axum::middleware::from_fn(local_only));
    }
    let listener = tokio::net::TcpListener::bind(opts.addr).await?;
    ready(listener.local_addr()?);
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
}

/// Rejects requests whose `Host` is not loopback, so a DNS-rebinding page cannot read the API.
async fn local_only(req: axum::extract::Request, next: axum::middleware::Next) -> Response {
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    let name = match host.rsplit_once(':') {
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) => h,
        _ => host,
    };
    if matches!(name, "127.0.0.1" | "localhost" | "[::1]") {
        next.run(req).await
    } else {
        (
            StatusCode::FORBIDDEN,
            "vigily answers on localhost only; start it with --host to serve other names",
        )
            .into_response()
    }
}

/// Refuses requests a web page could send: anything but `Content-Type: application/json` (a
/// page can send `text/plain` without a CORS preflight), and requests whose `Origin` is not this
/// server's own (a browser always names the page's origin on a cross-origin POST). Every route
/// that writes outside the web app's own forms (ingest, admin) checks this.
pub(crate) fn from_a_client(headers: &HeaderMap, what: &str) -> Result<(), ApiError> {
    from_a_client_as(headers, what, &["application/json"])
}

/// [`from_a_client`] for a route that accepts other media types as well (`""`: no
/// `Content-Type`, for a request without a body). The origin check stays.
pub(crate) fn from_a_client_as(
    headers: &HeaderMap,
    what: &str,
    media_types: &[&str],
) -> Result<(), ApiError> {
    let header = |n: header::HeaderName| headers.get(n).and_then(|v| v.to_str().ok());
    let ct = header(header::CONTENT_TYPE)
        .unwrap_or("")
        .to_ascii_lowercase();
    if ct.starts_with("application/x-protobuf") || ct.starts_with("application/protobuf") {
        return Err(ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "vigily accepts OTLP/HTTP with JSON encoding only; set the exporter's protocol to http/json",
        ));
    }
    let media = ct.split(';').next().unwrap_or("").trim();
    if !media_types.contains(&media) {
        return Err(ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            format!("send {what} with Content-Type: application/json"),
        ));
    }
    if let Some(origin) = header(header::ORIGIN) {
        let host = header(header::HOST).unwrap_or("");
        let own = !host.is_empty()
            && (origin == format!("http://{host}") || origin == format!("https://{host}"));
        if !own {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                format!("{what} are not accepted from web pages of another origin"),
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Types

fn row(e: &RunEntry, now: i64) -> RunRow {
    RunRow {
        run: summary_row(e, now),
        params: e.flat.clone(),
        metrics: e
            .series
            .iter()
            .map(|s| (s.key.clone(), MetricStat::from(&s.summary)))
            .collect(),
    }
}

fn summary_row(e: &RunEntry, now: i64) -> RunSummary {
    let r = &e.run;
    RunSummary {
        id: r.id.clone(),
        name: r.display_name(),
        has_name: r.name.is_some(),
        experiment: r.experiment.clone(),
        description: r.description.clone(),
        tags: r.tags.clone(),
        status: r.status,
        kind: e.kind().to_string(),
        target: e.target(),
        archived: r.archived,
        created_at: r.created_at,
        updated_at: r.updated_at,
        ended_at: r.ended_at,
        last_seen: e.last_seen(),
        stale: e.stale(now),
        duration: r.duration_secs(now),
    }
}

// ---------------------------------------------------------------------------------------------
// Handlers

async fn meta(State(s): State<Arc<AppState>>) -> ApiResult<Meta> {
    let snap = s.catalog.refresh_if_older(1_000_000).await?;
    Ok(Json(Meta {
        version: s.version.to_string(),
        repository: s.catalog.store().location(),
        runs: snap.runs.len(),
        running: snap
            .runs
            .iter()
            .filter(|e| e.run.status == RunStatus::Running)
            .count(),
        archived: snap.runs.iter().filter(|e| e.run.archived).count(),
        kinds: snap.runs.iter().fold(BTreeMap::new(), |mut m, e| {
            *m.entry(e.kind().to_string()).or_insert(0) += 1;
            m
        }),
        datasets: s.records.scan("ds/").await?.len(),
        schedules: s.config.schedule.len(),
        open_alerts: vigily_core::evals::alerts(&*s.records)
            .await?
            .iter()
            .filter(|a| a.acknowledged_at.is_none())
            .count(),
        traces: traces_api::count(&s).await?,
        refreshed_at: snap.refreshed_at,
        experiments: snap.experiments(),
        metrics: snap.metric_keys(),
        params: snap.param_keys(),
    }))
}

#[derive(Deserialize)]
struct RunsParams {
    #[serde(default)]
    q: String,
    sort: Option<String>,
    #[serde(default)]
    desc: bool,
    #[serde(default)]
    archived: bool,
    /// The first matching row to answer with.
    #[serde(default)]
    offset: usize,
    /// At most this many rows (1 to 10 000); every matching row when not set.
    limit: Option<usize>,
    #[serde(default)]
    fields: RunFields,
}

async fn runs(
    State(s): State<Arc<AppState>>,
    QueryParams(p): QueryParams<RunsParams>,
) -> Result<Response, ApiError> {
    if p.q.len() > MAX_QUERY {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            format!(
                "the filter is longer than {} KiB; shorten it",
                MAX_QUERY / 1024
            ),
        ));
    }
    let query = Query::parse(&p.q).map_err(|e| ApiError {
        position: Some(e.position),
        ..ApiError::new(StatusCode::BAD_REQUEST, e.message.clone())
    })?;
    if let Some(sort) = &p.sort {
        if Column::parse(sort).is_none() {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                format!("unknown sort column {sort:?}; use run:name, param:<key> or metric:<last|min|max>:<metric>"),
            ));
        }
    }
    let snap = s.catalog.refresh_if_older(1_000_000).await?;
    if p.limit.is_some_and(|l| l == 0 || l > 10_000) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "limit must be between 1 and 10 000",
        ));
    }
    let now = vigily_core::now_micros();
    let selected = snap.select(&query, p.sort.as_deref(), p.desc, p.archived);
    let page = selected
        .iter()
        .skip(p.offset)
        .take(p.limit.unwrap_or(usize::MAX));
    let (total, matched, refreshed_at) = (snap.runs.len(), selected.len(), snap.refreshed_at);
    Ok(match p.fields {
        RunFields::Full => Json(RunsResponse {
            total,
            matched,
            rows: page.map(|e| row(e, now)).collect(),
            refreshed_at,
        })
        .into_response(),
        RunFields::Summary => Json(RunsResponse {
            total,
            matched,
            rows: page.map(|e| summary_row(e, now)).collect(),
            refreshed_at,
        })
        .into_response(),
    })
}

async fn run(State(s): State<Arc<AppState>>, Path(id): Path<String>) -> ApiResult<RunDetail> {
    let snap = s.catalog.refresh_if_older(1_000_000).await?;
    let e = snap
        .get(&id)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, format!("no run with id {id}")))?;
    Ok(Json(RunDetail {
        row: row(e, vigily_core::now_micros()),
        params_tree: e.params.clone(),
        series: e.series.clone(),
    }))
}

async fn patch_run(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(patch): Json<RunPatch>,
) -> ApiResult<RunRow> {
    if patch.created_at.is_some() || patch.ended_at.is_some() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "times are written by the training script; change name, experiment, description, tags or archived",
        ));
    }
    let snap = s.catalog.refresh().await?;
    let mut patch = patch;
    match patch.status {
        None => {}
        // A run whose process died stays running; it can be marked aborted once it is stale.
        Some(RunStatus::Aborted) => {
            let e = snap.get(&id).ok_or_else(|| {
                ApiError::new(StatusCode::NOT_FOUND, format!("no run with id {id}"))
            })?;
            if !e.stale(vigily_core::now_micros()) {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    "only a running run with no heartbeat for 10 minutes can be marked aborted",
                ));
            }
            patch.ended_at = Some(e.last_seen());
        }
        Some(_) => {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "the status is written by the training script; a stale run can be marked aborted",
            ))
        }
    }
    s.catalog.store().update_run(&id, patch).await?;
    let snap = s.catalog.refresh().await?;
    let e = snap
        .get(&id)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, format!("no run with id {id}")))?;
    Ok(Json(row(e, vigily_core::now_micros())))
}

/// Deletes finished, failed or aborted runs with their parameters, series, points and evaluation
/// records. A running run is refused: its writer would keep writing to it, so it must be stopped
/// (finished or aborted) first.
async fn delete_runs(
    State(s): State<Arc<AppState>>,
    Json(req): Json<DeleteRuns>,
) -> ApiResult<Deleted> {
    if req.ids.is_empty() || req.ids.len() > 10_000 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "give between 1 and 10 000 run ids",
        ));
    }
    let snap = s.catalog.refresh().await?;
    let mut running = Vec::new();
    for id in &req.ids {
        match snap.get(id) {
            None => {
                return Err(ApiError::new(
                    StatusCode::NOT_FOUND,
                    format!("no run with id {id}"),
                ))
            }
            Some(e) if e.run.status == RunStatus::Running => running.push(e.run.display_name()),
            Some(_) => {}
        }
    }
    if !running.is_empty() {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            format!(
                "{} still running ({}); a running run can be deleted once its script finishes, or once it is marked aborted after its heartbeat stops",
                if running.len() == 1 { "1 run is" } else { "some runs are" },
                running.iter().take(3).cloned().collect::<Vec<_>>().join(", ")
            ),
        ));
    }
    let deleted = s.catalog.store().delete_runs(&req.ids).await?;
    for id in &req.ids {
        s.writes.writers.remove(id);
    }
    // Their traces went with them.
    *s.traces_count.lock() = None;
    *s.traces_cache.lock() = None;
    s.catalog.refresh().await?;
    tracing::info!(deleted, "deleted runs");
    Ok(Json(Deleted { deleted }))
}

fn last_point(xy: &Xy, range: Option<(f64, f64)>) -> Option<[f64; 2]> {
    let hi = range.map_or(f64::INFINITY, |r| r.1);
    let lo = range.map_or(f64::NEG_INFINITY, |r| r.0);
    xy.x.iter()
        .zip(&xy.y)
        .rev()
        .find(|(x, y)| **x <= hi && **x >= lo && y.is_finite())
        .map(|(x, y)| [*x, *y])
}

fn label(v: &vigily_core::query::Val) -> String {
    use vigily_core::query::Val;
    match v {
        Val::Null => "(none)".into(),
        Val::Num(n) => {
            let s = format!("{n}");
            if s.len() > 12 {
                format!("{n:.6e}")
            } else {
                s
            }
        }
        Val::Str(s) => s.clone(),
        Val::Bool(b) => b.to_string(),
    }
}

async fn chart(
    State(s): State<Arc<AppState>>,
    Json(req): Json<ChartRequest>,
) -> ApiResult<ChartResponse> {
    if req.runs.len() > 2_000 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "a chart takes at most 2 000 runs; narrow the selection",
        ));
    }
    let points = req.points.clamp(16, 4_000);
    let group_col = match &req.group_by {
        Some(g) => Some(Column::parse(g).ok_or_else(|| {
            ApiError::new(StatusCode::BAD_REQUEST, format!("unknown column {g:?}"))
        })?),
        None => None,
    };
    let snap = s.catalog.refresh_if_older(1_000_000).await?;
    let now = vigily_core::now_micros();

    // (entry, series id, summary count, finished)
    let mut found: Vec<(Arc<RunEntry>, u64, u64, bool)> = Vec::new();
    let mut missing = Vec::new();
    for id in &req.runs {
        match snap
            .get(id)
            .and_then(|e| e.series_by_key(&req.metric).map(|x| (e, x)))
        {
            Some((e, x)) => found.push((
                e.clone(),
                x.series.id,
                x.summary.count,
                e.run.status != RunStatus::Running,
            )),
            None => missing.push(id.clone()),
        }
    }

    // Raw points: from the cache for finished series, from storage for the rest.
    let mut raw: HashMap<u64, Arc<Points>> = HashMap::new();
    let mut to_read = Vec::new();
    {
        let cache = s.cache.lock();
        for (_, sid, count, done) in &found {
            match cache.get(*sid, *count).filter(|_| *done) {
                Some(p) => {
                    raw.insert(*sid, p);
                }
                None => to_read.push(*sid),
            }
        }
    }
    for chunk in to_read.chunks(512) {
        let read = s
            .catalog
            .store()
            .points(chunk, StepRange::default())
            .await?;
        let mut cache = s.cache.lock();
        for (sid, p) in read {
            let p = Arc::new(p);
            if let Some((_, _, count, true)) = found.iter().find(|f| f.1 == sid) {
                cache.put(sid, *count, p.clone());
            }
            raw.insert(sid, p);
        }
    }

    let empty = Arc::new(Points::default());
    let xys: Vec<Xy> = found
        .iter()
        .map(|(e, sid, _, _)| {
            let p = raw.get(sid).unwrap_or(&empty);
            Xy::new(x_values(p, req.x, e.run.created_at), &p.values)
        })
        .collect();
    let full = vigily_core::chart::domain(&xys, None).map(|(a, b)| [a, b]);
    let range = req.range.map(|[a, b]| (a, b));
    let groups_of: Vec<Option<String>> = found
        .iter()
        .map(|(e, ..)| group_col.as_ref().map(|c| label(&c.value(e, now))))
        .collect();

    if req.aggregate {
        let mut order: Vec<String> = Vec::new();
        let mut members: HashMap<String, Vec<Xy>> = HashMap::new();
        for (xy, g) in xys.into_iter().zip(&groups_of) {
            let g = g.clone().unwrap_or_else(|| "All runs".into());
            if !members.contains_key(&g) {
                order.push(g.clone());
            }
            members.entry(g).or_default().push(xy);
        }
        order.sort_by(|a, b| natural_cmp(a, b));
        let sets: Vec<Vec<Xy>> = order
            .iter()
            .map(|g| members.remove(g).unwrap_or_default())
            .collect();
        let sizes: Vec<usize> = sets.iter().map(Vec::len).collect();
        let (x, bands) = aggregate(&sets, range, points);
        return Ok(Json(ChartResponse {
            metric: req.metric,
            x_axis: req.x,
            x,
            exact: false,
            domain: full,
            series: vec![],
            groups: order
                .into_iter()
                .zip(bands)
                .zip(sizes)
                .map(|((label, band), runs)| ChartGroup { label, runs, band })
                .collect(),
            missing,
        }));
    }

    let lasts: Vec<Option<[f64; 2]>> = xys.iter().map(|xy| last_point(xy, range)).collect();
    let aligned = align(&xys, range, points);
    Ok(Json(ChartResponse {
        metric: req.metric,
        x_axis: req.x,
        x: aligned.x,
        exact: aligned.exact,
        domain: full,
        series: found
            .iter()
            .zip(aligned.ys)
            .zip(groups_of)
            .zip(lasts)
            .map(|((((e, ..), y), group), last)| ChartSeries {
                run_id: e.run.id.clone(),
                name: e.run.display_name(),
                group,
                y,
                last,
            })
            .collect(),
        groups: vec![],
        missing,
    }))
}

fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    match (a.parse::<f64>(), b.parse::<f64>()) {
        (Ok(x), Ok(y)) => x.total_cmp(&y),
        _ => a.to_lowercase().cmp(&b.to_lowercase()),
    }
}

/// The embedded web app: a file by path, or `index.html` for client-side routes.
async fn asset(uri: axum::http::Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let find = |p: &str| {
        vigily_ui::ASSETS
            .iter()
            .find(|(n, _)| *n == p)
            .map(|(_, b)| *b)
    };
    let (name, body) = match find(path) {
        Some(b) if !path.is_empty() => (path, b),
        _ if path.starts_with("api/v1/") || path == "api/v1" => {
            return ApiError::new(StatusCode::NOT_FOUND, format!("no such endpoint: /{path}"))
                .into_response();
        }
        _ if path.starts_with("api/") => {
            return ApiError::new(StatusCode::NOT_FOUND, format!("no such endpoint: /{path}"))
                .legacy_response();
        }
        _ if path.starts_with("assets/") => {
            return (StatusCode::NOT_FOUND, "not found").into_response();
        }
        _ => ("index.html", find("index.html").unwrap_or_default()),
    };
    let mime = match name.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript",
        Some("css") => "text/css",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("json") => "application/json",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    };
    // Bundled assets carry a content hash in their name; the page itself must stay fresh.
    let cache = if name.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, mime),
            (header::CACHE_CONTROL, cache),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        body,
    )
        .into_response()
}
