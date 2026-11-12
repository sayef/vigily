//! The tracing API: ingest (vigily JSON and OTLP/HTTP JSON), the traces list and one trace.
//!
//! | Method and path              | What it does |
//! |------------------------------|--------------|
//! | `POST /api/v1/traces/ingest` | Stores a batch of spans (`{"spans": [...]}`), see [`vigily_core::traces::SpanIn`] |
//! | `POST /v1/traces`            | The same for an OpenTelemetry OTLP/HTTP JSON export |
//! | `GET /api/v1/traces`         | Trace summaries, newest first, filtered by run, model, status, time and name |
//! | `GET /api/v1/traces/{id}`    | One trace with its spans |
//! | `DELETE /api/v1/traces/{id}` | Deletes one trace |
//! | `POST /api/v1/traces/delete` | Deletes traces by id, and erases them from storage with `erase` |
//! | `GET /api/v1/tracing/policy` | The payload policy of a project, for clients to apply before sending |

use std::collections::BTreeSet;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Query as QueryParams, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::Value;
use vigily_api::traces::{
    DeleteTraces, IngestBody, Policy, RunRef, Totals, TraceDeleted, TraceResponse, TraceRow,
    TracesDeleted, TracesResponse, TracingPolicy,
};
use vigily_core::traces::{self, IngestReport, SpanIn, TraceSummary};

use crate::auth::Scoped;
use crate::{ApiError, AppState};

type ApiResult<T> = Result<Json<T>, ApiError>;

/// The largest ingest request body.
pub const MAX_INGEST_BYTES: usize = 32 * 1024 * 1024;

/// The tracing routes, relative to the API root, by the scope they need.
pub fn routes() -> Scoped {
    Scoped::default()
        .ingest(Router::new().route(
            "/traces/ingest",
            post(ingest).layer(DefaultBodyLimit::max(MAX_INGEST_BYTES)),
        ))
        .read(
            Router::new()
                .route("/traces", get(list))
                .route("/traces/{id}", get(one)),
        )
        .admin(Router::new().route("/traces/{id}", delete(delete_one)))
}

/// Tracing routes added with `/api/v1`, which have no alias under `/api`.
pub fn v1_routes() -> Scoped {
    Scoped::default()
        .read(Router::new().route("/tracing/policy", get(policy)))
        .admin(Router::new().route("/traces/delete", post(delete_many)))
}

/// OpenTelemetry's OTLP/HTTP endpoint, at the root where exporters expect it. It needs the
/// `ingest` scope like every other write.
pub fn otlp_routes() -> Scoped {
    Scoped::default().ingest(Router::new().route(
        "/v1/traces",
        post(ingest_otlp).layer(DefaultBodyLimit::max(MAX_INGEST_BYTES)),
    ))
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

async fn ingest(
    State(s): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<IngestReport> {
    crate::from_a_client(&headers, "spans")?;
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
    crate::from_a_client(&headers, "spans")?;
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

async fn delete_one(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<TraceDeleted> {
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

/// Deletes traces by id; with `erase`, purges storage afterwards so their spans are erased from the
/// files too (as `vigily traces delete --erase` does).
async fn delete_many(
    State(s): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<TracesDeleted> {
    crate::from_a_client(&headers, "admin requests")?;
    let req: DeleteTraces = crate::admin_api::body(&body)?;
    if req.ids.is_empty() || req.ids.len() > 10_000 {
        return Err(bad("give between 1 and 10 000 trace ids"));
    }
    let mut deleted = 0;
    let mut missing = Vec::new();
    {
        let _one_at_a_time = s.ingest_lock.lock().await;
        for id in &req.ids {
            if traces::delete(&*s.records, id).await? {
                deleted += 1;
            } else {
                missing.push(id.clone());
            }
        }
    }
    *s.traces_cache.lock() = None;
    *s.traces_count.lock() = None;
    let purge = if req.erase {
        Some(crate::admin_api::run_purge(&s, req.force).await?)
    } else {
        None
    };
    Ok(Json(TracesDeleted {
        deleted,
        missing,
        purge,
    }))
}

#[derive(Deserialize)]
struct PolicyParams {
    project: Option<String>,
}

async fn policy(
    State(s): State<Arc<AppState>>,
    QueryParams(p): QueryParams<PolicyParams>,
) -> ApiResult<TracingPolicy> {
    let t = &s.config.tracing;
    let project = p.project.filter(|p| !p.is_empty());
    let (payloads, max_field_bytes) = t.policy(project.as_deref());
    let (strict, strict_bytes) = t.strictest();
    Ok(Json(TracingPolicy {
        project,
        policy: Policy {
            payloads,
            max_field_bytes,
        },
        unknown_project: Policy {
            payloads: strict,
            max_field_bytes: strict_bytes,
        },
        currency: t.currency.clone(),
    }))
}
