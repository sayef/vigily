//! The tracing API: ingest (vigily JSON and OTLP/HTTP JSON), the traces list and one trace.
//!
//! | Method and path              | What it does |
//! |------------------------------|--------------|
//! | `POST /api/v1/traces/ingest` | Stores a batch of spans (`{"spans": [...]}`), see [`vigily_core::traces::SpanIn`] |
//! | `POST /v1/traces`            | The same for an OpenTelemetry OTLP/HTTP JSON export |
//! | `GET /api/v1/traces`         | Trace summaries, newest first, filtered by run, model, status, time and name |
//! | `GET /api/v1/traces/{id}`    | One trace with its spans |
//! | `DELETE /api/v1/traces/{id}` | Deletes one trace |

use std::collections::BTreeSet;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Query as QueryParams, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::Value;
use vigily_api::traces::{
    IngestBody, RunRef, Totals, TraceDeleted, TraceResponse, TraceRow, TracesResponse,
};
use vigily_core::traces::{self, IngestReport, SpanIn, TraceSummary};

use crate::{ApiError, AppState};

type ApiResult<T> = Result<Json<T>, ApiError>;

/// The largest ingest request body.
pub const MAX_INGEST_BYTES: usize = 32 * 1024 * 1024;

/// The tracing routes, relative to the API root.
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route(
            "/traces/ingest",
            post(ingest).layer(DefaultBodyLimit::max(MAX_INGEST_BYTES)),
        )
        .route("/traces", get(list))
        .route("/traces/{id}", get(one).delete(delete))
}

/// OpenTelemetry's OTLP/HTTP endpoint, at the root where exporters expect it.
pub fn otlp_routes() -> Router<Arc<AppState>> {
    Router::new().route(
        "/v1/traces",
        post(ingest_otlp).layer(DefaultBodyLimit::max(MAX_INGEST_BYTES)),
    )
}

fn bad(m: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, m)
}

async fn store(s: &AppState, spans: Vec<SpanIn>) -> Result<IngestReport, ApiError> {
    let snap = s.catalog.snapshot();
    let experiment_of = move |run: &str| snap.get(run).and_then(|e| e.run.experiment.clone());
    let report = {
        let _one_at_a_time = s.ingest_lock.lock().await;
        traces::ingest(&*s.records, &s.config.tracing, spans, &experiment_of).await?
    };
    *s.traces_cache.lock() = None;
    if let Some((_, n)) = s.traces_count.lock().as_mut() {
        *n += report.new_traces;
    }
    Ok(report)
}

/// Refuses requests a web page could send: anything but `Content-Type: application/json` (a
/// page can send `text/plain` without a CORS preflight), and requests whose `Origin` is not this
/// server's own (a browser always names the page's origin on a cross-origin POST).
fn from_a_client(headers: &HeaderMap) -> Result<(), ApiError> {
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
    if media != "application/json" {
        return Err(ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "send spans with Content-Type: application/json",
        ));
    }
    if let Some(origin) = header(header::ORIGIN) {
        let host = header(header::HOST).unwrap_or("");
        let own = !host.is_empty()
            && (origin == format!("http://{host}") || origin == format!("https://{host}"));
        if !own {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "spans are not accepted from web pages of another origin",
            ));
        }
    }
    Ok(())
}

async fn ingest(
    State(s): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<IngestReport> {
    from_a_client(&headers)?;
    let body: IngestBody = serde_json::from_slice(&body)
        .map_err(|e| bad(format!("expected {{\"spans\": [...]}}: {e}")))?;
    Ok(Json(store(&s, body.spans).await?))
}

/// OTLP/HTTP with JSON encoding. Protobuf is refused with 415 and a hint.
async fn ingest_otlp(
    State(s): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<Value>, ApiError> {
    from_a_client(&headers)?;
    let v: Value = serde_json::from_slice(&body).map_err(|e| bad(format!("not JSON: {e}")))?;
    let spans = traces::from_otlp(&v)?;
    store(&s, spans).await?;
    // An empty ExportTraceServiceResponse: everything was accepted.
    Ok(Json(serde_json::json!({})))
}

/// Every summary, cached until the trace table's version changes.
async fn summaries(s: &AppState) -> Result<Arc<Vec<TraceSummary>>, ApiError> {
    let version = s
        .records
        .storage_version(traces::TRACE)
        .await
        .ok()
        .flatten();
    if let Some((v, list)) = s.traces_cache.lock().as_ref() {
        if version.is_some() && *v == version {
            return Ok(list.clone());
        }
    }
    let list = Arc::new(traces::list(&*s.records).await?);
    *s.traces_cache.lock() = Some((version, list.clone()));
    Ok(list)
}

/// The number of stored traces (for the sidebar).
/// How long a counted number of traces is trusted before it is counted again (ingests add to it;
/// deletions reset it).
const COUNT_FOR_US: i64 = 60_000_000;

/// The number of stored traces (for the sidebar). Counted from storage at most once a minute;
/// ingests in between add their new traces, so continuous ingest does not rescan the table.
pub(crate) async fn count(s: &AppState) -> Result<usize, ApiError> {
    let now = vigily_core::now_micros();
    if let Some((at, n)) = *s.traces_count.lock() {
        if now - at < COUNT_FOR_US {
            return Ok(n);
        }
    }
    let n = summaries(s).await?.len();
    *s.traces_count.lock() = Some((now, n));
    Ok(n)
}

#[derive(Deserialize)]
struct ListParams {
    run: Option<String>,
    model: Option<String>,
    /// `ok` or `error`.
    status: Option<String>,
    /// UTC microseconds: traces that started at or after.
    since: Option<i64>,
    /// UTC microseconds: traces that started before.
    until: Option<i64>,
    /// Part of the trace name, case-insensitive.
    #[serde(default)]
    q: String,
    eval_item: Option<String>,
    #[serde(default = "default_limit")]
    limit: usize,
    #[serde(default)]
    offset: usize,
}

fn default_limit() -> usize {
    200
}

fn run_name(snap: &vigily_core::Snapshot, id: Option<&str>) -> Option<String> {
    id.and_then(|i| snap.get(i)).map(|e| e.run.display_name())
}

async fn list(
    State(s): State<Arc<AppState>>,
    QueryParams(p): QueryParams<ListParams>,
) -> ApiResult<TracesResponse> {
    let all = summaries(&s).await?;
    let snap = s.catalog.snapshot();
    let q = p.q.to_lowercase();
    let status = p.status.as_deref().filter(|s| !s.is_empty());
    let matching: Vec<&TraceSummary> = all
        .iter()
        .filter(|t| {
            p.run
                .as_deref()
                .is_none_or(|r| r.is_empty() || t.run_id.as_deref() == Some(r))
                && p.model
                    .as_deref()
                    .is_none_or(|m| m.is_empty() || t.models.iter().any(|x| x == m))
                && status.is_none_or(|st| t.status == st)
                && p.since.is_none_or(|x| t.start_time >= x)
                && p.until.is_none_or(|x| t.start_time < x)
                && (q.is_empty() || t.name.to_lowercase().contains(&q) || t.id.starts_with(&q))
                && p.eval_item
                    .as_deref()
                    .is_none_or(|i| t.eval_item.as_deref() == Some(i))
        })
        .collect();
    let mut durations: Vec<i64> = matching.iter().map(|t| t.duration_us()).collect();
    durations.sort_unstable();
    let pct = |f: f64| -> Option<i64> {
        (!durations.is_empty())
            .then(|| durations[((durations.len() - 1) as f64 * f).round() as usize])
    };
    let mut cost = std::collections::BTreeMap::new();
    for t in &matching {
        if let Some(c) = t.cost {
            *cost.entry(t.currency.clone()).or_insert(0.0) += c;
        }
    }
    let totals = Totals {
        traces: matching.len(),
        errors: matching.iter().filter(|t| t.status == "error").count(),
        input_tokens: matching.iter().map(|t| t.input_tokens).sum(),
        output_tokens: matching.iter().map(|t| t.output_tokens).sum(),
        cached_tokens: matching.iter().map(|t| t.cached_tokens).sum(),
        cost,
        unpriced: matching.iter().map(|t| t.unpriced).sum(),
        p50_us: pct(0.5),
        p95_us: pct(0.95),
    };
    let limit = p.limit.clamp(1, 2000);
    let rows = matching
        .iter()
        .skip(p.offset)
        .take(limit)
        .map(|t| TraceRow {
            run_name: run_name(&snap, t.run_id.as_deref()),
            duration_us: t.duration_us(),
            summary: (*t).clone(),
        })
        .collect();
    let models: BTreeSet<String> = all.iter().flat_map(|t| t.models.iter().cloned()).collect();
    let run_ids: BTreeSet<&str> = all.iter().filter_map(|t| t.run_id.as_deref()).collect();
    let runs = run_ids
        .into_iter()
        .map(|id| RunRef {
            id: id.to_string(),
            name: run_name(&snap, Some(id)).unwrap_or_else(|| id.chars().take(8).collect()),
        })
        .collect();
    Ok(Json(TracesResponse {
        total: matching.len(),
        rows,
        totals,
        models: models.into_iter().collect(),
        runs,
        payloads: s.config.tracing.payloads,
        max_field_bytes: s.config.tracing.max_field_bytes,
        currency: s.config.tracing.currency.clone(),
    }))
}

async fn one(State(s): State<Arc<AppState>>, Path(id): Path<String>) -> ApiResult<TraceResponse> {
    let Some((summary, spans)) = traces::get(&*s.records, &id).await? else {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            format!("no trace with id {id}; it may have been deleted with its run"),
        ));
    };
    let snap = s.catalog.snapshot();
    let max_field_bytes = match &summary.project {
        Some(p) => s.config.tracing.policy(Some(p)).1,
        None => s.config.tracing.strictest().1,
    };
    Ok(Json(TraceResponse {
        trace: TraceRow {
            run_name: run_name(&snap, summary.run_id.as_deref()),
            duration_us: summary.duration_us(),
            summary,
        },
        spans,
        max_field_bytes,
    }))
}

async fn delete(State(s): State<Arc<AppState>>, Path(id): Path<String>) -> ApiResult<TraceDeleted> {
    let deleted = {
        let _one_at_a_time = s.ingest_lock.lock().await;
        traces::delete(&*s.records, &id).await?
    };
    if !deleted {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            format!("no trace with id {id}"),
        ));
    }
    *s.traces_cache.lock() = None;
    *s.traces_count.lock() = None;
    Ok(Json(TraceDeleted { deleted }))
}
