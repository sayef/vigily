//! The write path for clients: creating, resuming and finishing runs, eval results and feature
//! samples, dataset commits, batched ingest and flushing. Every route here refuses requests a web
//! page could send (see [`crate::from_a_client`]).
//!
//! | Method and path                          | What it does |
//! |------------------------------------------|--------------|
//! | `POST /api/v1/runs`                      | Creates a run with the client's id; an existing id answers the existing run |
//! | `POST /api/v1/runs/{id}/resume`          | Opens a run for writing: its metadata, parameters and each series' last step |
//! | `POST /api/v1/runs/{id}/finish`          | Ends a run, after its results and features are in storage |
//! | `POST /api/v1/runs/{id}/results`         | Stores per-item eval results |
//! | `POST /api/v1/runs/{id}/features`        | Stores input feature samples |
//! | `POST /api/v1/datasets/{name}/versions`  | Commits a dataset version (requires `Idempotency-Key`) |
//! | `POST /api/v1/ingest`                    | Applies batches of operations in order ([`vigily_api::ingest`]) |
//! | `POST /api/v1/flush`                     | Commits buffered writes and refreshes the catalog |
//!
//! Bodies may be gzip-compressed and are limited to [`crate::writers::MAX_BODY`] after
//! decompression. Ingest bodies in flight share [`crate::writers::INGEST_BUDGET`]; above it ingest
//! answers 503 `overloaded` with `Retry-After`.

use std::collections::BTreeSet;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;
use vigily_api::code;
use vigily_api::evals::{CommitDataset, EvalResult, FeatureSample, PutFeatures, PutResults};
use vigily_api::ingest::{IngestBatch, IngestReport, IngestReports, Op};
use vigily_api::runs::{
    CreateRun, CreatedRun, FinishRun, Flushed, ResumedRun, ResumedSeries, SeriesPoints, Stored,
};
use vigily_core::{evals, Error as CoreError, NewRun, Run, RunStatus, RunWriter};

use crate::auth::Scoped;
use crate::writers::WriteLimits;
use crate::{ApiError, AppState};

/// Bodies larger than this are decoded on a blocking thread, off the request-serving ones.
const DECODE_INLINE: usize = 1024 * 1024;

/// The header that makes a dataset commit (required) or an ingest request (optional) idempotent.
pub const IDEMPOTENCY_KEY: &str = "idempotency-key";

/// The longest accepted idempotency key.
const MAX_KEY: usize = 256;

const NDJSON: &str = "application/x-ndjson";

/// The write routes, relative to the API root; they need the `ingest` scope.
pub fn routes(state: &Arc<AppState>, limits: WriteLimits) -> Scoped {
    let ingest = Router::new()
        .route("/ingest", post(ingest))
        .layer(axum::middleware::from_fn_with_state(state.clone(), budget));
    let writes = Router::new()
        .route("/runs", post(create))
        .route("/runs/{id}/resume", post(resume))
        .route("/runs/{id}/finish", post(finish_route))
        .route("/runs/{id}/results", post(results_route))
        .route("/runs/{id}/features", post(features_route))
        .route("/datasets/{name}/versions", post(commit_dataset))
        .route("/flush", post(flush))
        .merge(ingest)
        .layer(DefaultBodyLimit::max(limits.max_body))
        .layer(tower_http::decompression::RequestDecompressionLayer::new());
    Scoped::default().ingest(writes)
}

/// Parses a JSON body.
fn parse<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, ApiError> {
    serde_json::from_slice(bytes).map_err(|e| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            format!("the request body is not valid JSON for this route: {e}"),
        )
    })
}

/// An error writing to run `id`: storage reports a deleted run as not found.
fn run_error(id: &str, e: CoreError) -> ApiError {
    match e {
        CoreError::NotFound(_) => run_deleted(id),
        other => other.into(),
    }
}

fn run_deleted(id: &str) -> ApiError {
    ApiError::new(
        StatusCode::GONE,
        format!("run {id} was deleted or never created; nothing more is recorded for it"),
    )
    .with_code(code::RUN_DELETED)
}

fn internal(e: impl std::fmt::Display) -> ApiError {
    ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

/// A run id as clients choose them: a UUIDv7 as 32 lower-case hex digits.
fn check_id(id: &str) -> Result<(), ApiError> {
    if id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        Ok(())
    } else {
        Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            format!("run id {id:?} is not a UUIDv7 in 32 lower-case hex digits (no dashes)"),
        ))
    }
}

// ---------------------------------------------------------------------------------------------
// Operations shared by the routes and ingest

/// The open writer of run `id`, opening it when needed.
pub(crate) async fn writer(s: &AppState, id: &str) -> Result<Arc<dyn RunWriter>, ApiError> {
    if let Some(w) = s.writes.writers.get(id) {
        return Ok(w);
    }
    let w: Arc<dyn RunWriter> = Arc::from(
        s.catalog
            .store()
            .open_run(id)
            .await
            .map_err(|e| run_error(id, e))?,
    );
    s.writes.writers.insert(id, w.clone(), false);
    Ok(w)
}

/// Stores a heartbeat row when one is due for this run (at most one per two minutes).
fn heartbeat(s: &AppState, id: &str, w: &dyn RunWriter) -> Result<(), ApiError> {
    if s.writes.writers.heartbeat_due(id) {
        w.heartbeat().map_err(|e| run_error(id, e))?;
    }
    Ok(())
}

/// Creates a run, or answers the existing one with this id.
pub(crate) async fn create_run(s: &AppState, req: CreateRun) -> Result<CreatedRun, ApiError> {
    check_id(&req.id)?;
    let _one = s.writes.create_lock.lock().await;
    if let Some(w) = s.writes.writers.get(&req.id) {
        return Ok(CreatedRun {
            created: false,
            run: w.info(),
        });
    }
    match s.catalog.store().open_run(&req.id).await {
        Ok(w) => {
            let w: Arc<dyn RunWriter> = Arc::from(w);
            s.writes.writers.insert(&req.id, w.clone(), false);
            return Ok(CreatedRun {
                created: false,
                run: w.info(),
            });
        }
        Err(CoreError::NotFound(_)) => {}
        Err(e) => return Err(e.into()),
    }
    let id = req.id.clone();
    let w: Arc<dyn RunWriter> = Arc::from(
        s.catalog
            .store()
            .create_run(NewRun {
                id: Some(req.id),
                name: req.name,
                experiment: req.experiment,
                description: req.description,
                tags: req.tags,
                created_at: req.created_at,
                kind: req.kind,
                target: req.target,
            })
            .await?,
    );
    s.writes.writers.insert(&id, w.clone(), true);
    Ok(CreatedRun {
        created: true,
        run: w.info(),
    })
}

/// Ends a run once everything written before is in storage: results and feature samples first,
/// so a finished run never lacks them.
pub(crate) async fn finish_run(
    s: &AppState,
    id: &str,
    status: RunStatus,
    ended_at: Option<i64>,
) -> Result<Run, ApiError> {
    if status == RunStatus::Running {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "finish a run as finished, failed or aborted",
        ));
    }
    let w = writer(s, id).await?;
    s.records.flush().await?;
    // `finish` commits the run's buffered points together with its new status (one storage
    // commit instead of two per run).
    w.finish(status, ended_at)
        .await
        .map_err(|e| run_error(id, e))?;
    s.writes.writers.remove(id);
    Ok(w.info())
}

/// Checks that every run the records name exists (and counts as their heartbeat).
async fn check_runs<'a>(s: &AppState, runs: impl Iterator<Item = &'a str>) -> Result<(), ApiError> {
    for id in runs.collect::<BTreeSet<_>>() {
        let w = writer(s, id).await?;
        heartbeat(s, id, &*w)?;
    }
    Ok(())
}

pub(crate) async fn put_results(s: &AppState, results: Vec<EvalResult>) -> Result<usize, ApiError> {
    check_runs(s, results.iter().map(|r| r.run_id.as_str())).await?;
    let n = results.len();
    evals::put_results(&*s.records, results).await?;
    Ok(n)
}

pub(crate) async fn put_features(
    s: &AppState,
    samples: Vec<FeatureSample>,
) -> Result<usize, ApiError> {
    check_runs(s, samples.iter().map(|r| r.run_id.as_str())).await?;
    let n = samples.len();
    evals::put_features(&*s.records, samples).await?;
    Ok(n)
}

fn track(s: &AppState, points: &SeriesPoints) -> Result<usize, ApiError> {
    if points.name.trim().is_empty() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "a series needs a metric name",
        ));
    }
    let w = s
        .writes
        .writers
        .get(&points.run_id)
        .ok_or_else(|| run_deleted(&points.run_id))?;
    w.track_batch(points)
        .map_err(|e| run_error(&points.run_id, e))?;
    heartbeat(s, &points.run_id, &*w)?;
    Ok(points.steps.len())
}

/// Applies one batch's operations in order.
async fn apply(s: &AppState, batch: IngestBatch) -> Result<IngestReport, ApiError> {
    let mut report = IngestReport {
        writer: batch.writer,
        seq: batch.seq,
        ..Default::default()
    };
    for (i, op) in batch.ops.into_iter().enumerate() {
        let what = op_name(&op);
        let done: Result<(), ApiError> = async {
            match op {
                Op::Create(req) => {
                    if create_run(s, req).await?.created {
                        report.runs_created += 1;
                    }
                }
                Op::Params { run_id, params } => {
                    let w = writer(s, &run_id).await?;
                    if !params.is_object() {
                        return Err(ApiError::new(
                            StatusCode::BAD_REQUEST,
                            "parameters must be a JSON object",
                        ));
                    }
                    w.set_params(&params).map_err(|e| run_error(&run_id, e))?;
                    heartbeat(s, &run_id, &*w)?;
                }
                Op::Points(points) => {
                    writer(s, &points.run_id).await?;
                    report.points += track(s, &points)?;
                }
                Op::Update { run_id, patch } => {
                    let w = writer(s, &run_id).await?;
                    w.update(patch).await.map_err(|e| run_error(&run_id, e))?;
                    heartbeat(s, &run_id, &*w)?;
                }
                Op::Heartbeat { run_id } => {
                    // Clients send these on their own schedule (the Python SDK every two
                    // minutes): each is stored, and restarts the implicit heartbeat's clock.
                    let w = writer(s, &run_id).await?;
                    s.writes.writers.heartbeat_now(&run_id);
                    w.heartbeat().map_err(|e| run_error(&run_id, e))?;
                }
                Op::Finish {
                    run_id,
                    status,
                    ended_at,
                } => {
                    finish_run(s, &run_id, status, ended_at).await?;
                }
                Op::Results { results } => report.results += put_results(s, results).await?,
                Op::Features { samples } => report.features += put_features(s, samples).await?,
            }
            Ok(())
        }
        .await;
        if let Err(mut e) = done {
            e.message = format!("operation {} ({what}): {}", i + 1, e.message);
            return Err(e);
        }
        report.ops += 1;
    }
    Ok(report)
}

fn op_name(op: &Op) -> &'static str {
    match op {
        Op::Create(_) => "create",
        Op::Params { .. } => "params",
        Op::Points(_) => "points",
        Op::Update { .. } => "update",
        Op::Heartbeat { .. } => "heartbeat",
        Op::Finish { .. } => "finish",
        Op::Results { .. } => "results",
        Op::Features { .. } => "features",
    }
}

/// Applies a batch unless its writer already did: one batch of a writer at a time, on a blocking
/// thread (tracking points is synchronous and may commit to storage).
async fn apply_once(s: &Arc<AppState>, batch: IngestBatch) -> Result<IngestReport, ApiError> {
    if batch.writer.trim().is_empty() || batch.writer.len() > 128 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "a batch needs a writer id of 1 to 128 characters",
        ));
    }
    let seqs = s.writes.idempotency.writer(&batch.writer);
    let mut seq = seqs.lock_owned().await;
    if let Some(r) = seq.replay(&batch.writer, batch.seq) {
        return Ok(r);
    }
    let (st, rt) = (s.clone(), tokio::runtime::Handle::current());
    let report = tokio::task::spawn_blocking(move || rt.block_on(apply(&st, batch)))
        .await
        .map_err(internal)??;
    seq.applied(&report);
    Ok(report)
}

// ---------------------------------------------------------------------------------------------
// Handlers

async fn create(
    State(s): State<Arc<AppState>>,
    headers: HeaderMap,
    bytes: Bytes,
) -> Result<Response, ApiError> {
    crate::from_a_client(&headers, "run writes")?;
    let created = create_run(&s, parse(&bytes)?).await?;
    s.refresh_now.notify_one();
    let status = if created.created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(created)).into_response())
}

async fn resume(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<ResumedRun>, ApiError> {
    crate::from_a_client_as(&headers, "run writes", &["application/json", ""])?;
    let w = writer(&s, &id).await?;
    // A run this engine is still writing (created a moment ago, or written by another process)
    // is in the catalog once its buffered rows are.
    w.flush().await.map_err(|e| run_error(&id, e))?;
    let snap = s.catalog.refresh().await?;
    let e = snap.get(&id).ok_or_else(|| run_deleted(&id))?;
    Ok(Json(ResumedRun {
        run: w.info(),
        params: e.params.clone(),
        series: e
            .series
            .iter()
            .map(|x| ResumedSeries {
                name: x.series.name.clone(),
                context: x.series.context.clone(),
                key: x.key.clone(),
                last_step: x.summary.last_step,
            })
            .collect(),
    }))
}

async fn finish_route(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    bytes: Bytes,
) -> Result<Json<Run>, ApiError> {
    crate::from_a_client(&headers, "run writes")?;
    let req: FinishRun = parse(&bytes)?;
    let run = finish_run(&s, &id, req.status, req.ended_at).await?;
    s.refresh_now.notify_one();
    Ok(Json(run))
}

async fn results_route(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    bytes: Bytes,
) -> Result<Json<Stored>, ApiError> {
    crate::from_a_client(&headers, "eval results")?;
    let req: PutResults = parse(&bytes)?;
    if let Some(r) = req.results.iter().find(|r| r.run_id != id) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            format!("a result names run {}, not {id}", r.run_id),
        ));
    }
    let stored = put_results(&s, req.results).await?;
    Ok(Json(Stored { stored }))
}

async fn features_route(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    bytes: Bytes,
) -> Result<Json<Stored>, ApiError> {
    crate::from_a_client(&headers, "feature samples")?;
    let req: PutFeatures = parse(&bytes)?;
    if let Some(r) = req.samples.iter().find(|r| r.run_id != id) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            format!("a feature sample names run {}, not {id}", r.run_id),
        ));
    }
    let stored = put_features(&s, req.samples).await?;
    Ok(Json(Stored { stored }))
}

fn idempotency_key(headers: &HeaderMap) -> Result<Option<String>, ApiError> {
    let Some(v) = headers.get(IDEMPOTENCY_KEY) else {
        return Ok(None);
    };
    match v.to_str() {
        Ok(k) if !k.trim().is_empty() && k.len() <= MAX_KEY => Ok(Some(k.trim().to_string())),
        _ => Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            format!("Idempotency-Key must be 1 to {MAX_KEY} visible ASCII characters"),
        )),
    }
}

/// Marks an answer replayed from an earlier request with the same key.
fn replayed<T: Serialize>(status: StatusCode, body: T) -> Response {
    let mut res = (status, Json(body)).into_response();
    res.headers_mut()
        .insert("idempotent-replayed", HeaderValue::from_static("true"));
    res
}

async fn commit_dataset(
    State(s): State<Arc<AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    bytes: Bytes,
) -> Result<Response, ApiError> {
    crate::from_a_client(&headers, "dataset commits")?;
    let key = idempotency_key(&headers)?.ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "a dataset commit needs an Idempotency-Key header (a new random value per commit, reused on retries)",
        )
    })?;
    let req: CommitDataset = parse(&bytes)?;
    let _one = s.writes.commit_lock.lock().await;
    let (version, again) = evals::commit_dataset_once(&*s.records, &key, &name, req).await?;
    Ok(if again {
        replayed(StatusCode::OK, version)
    } else {
        (StatusCode::CREATED, Json(version)).into_response()
    })
}

async fn flush(
    State(s): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<Flushed>, ApiError> {
    crate::from_a_client_as(&headers, "flush requests", &["application/json", ""])?;
    s.catalog.store().flush().await?;
    s.records.flush().await?;
    let snap = s.catalog.refresh().await?;
    Ok(Json(Flushed {
        refreshed_at: snap.refreshed_at,
    }))
}

/// Decodes an ingest body: one batch as JSON, or one per line as NDJSON.
fn decode(bytes: &[u8], ndjson: bool) -> Result<Vec<IngestBatch>, ApiError> {
    if !ndjson {
        return Ok(vec![parse(bytes)?]);
    }
    let mut out = Vec::new();
    for (n, line) in bytes.split(|b| *b == b'\n').enumerate() {
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        out.push(serde_json::from_slice(line).map_err(|e| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                format!("line {} is not an ingest batch: {e}", n + 1),
            )
        })?);
    }
    Ok(out)
}

async fn ingest(
    State(s): State<Arc<AppState>>,
    headers: HeaderMap,
    bytes: Bytes,
) -> Result<Response, ApiError> {
    crate::from_a_client_as(&headers, "ingest batches", &["application/json", NDJSON])?;
    let key = idempotency_key(&headers)?;
    if let Some(answer) = key.as_deref().and_then(|k| s.writes.idempotency.answer(k)) {
        return Ok(replayed(StatusCode::OK, answer));
    }
    let ndjson = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.to_ascii_lowercase().starts_with(NDJSON));
    let batches = if bytes.len() > DECODE_INLINE {
        tokio::task::spawn_blocking(move || decode(&bytes, ndjson))
            .await
            .map_err(internal)??
    } else {
        decode(&bytes, ndjson)?
    };
    let mut reports = Vec::with_capacity(batches.len());
    let mut failed = None;
    for b in batches {
        match apply_once(&s, b).await {
            Ok(r) => reports.push(r),
            Err(e) => {
                failed = Some(e);
                break;
            }
        }
    }
    if !reports.is_empty() {
        s.refresh_now.notify_one();
    }
    if let Some(e) = failed {
        return Err(e);
    }
    let answer: Value = if ndjson {
        serde_json::to_value(IngestReports { batches: reports })
    } else {
        serde_json::to_value(reports.pop().unwrap_or_default())
    }
    .map_err(internal)?;
    if let Some(k) = key {
        s.writes.idempotency.remember(&k, answer.clone());
    }
    Ok(Json(answer).into_response())
}

/// Holds an ingest request's share of the in-flight budget while it runs; answers 503 with
/// `Retry-After` when the budget is spent.
async fn budget(
    State(s): State<Arc<AppState>>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let encoded = req
        .headers()
        .get(header::CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| !v.eq_ignore_ascii_case("identity"));
    let declared = req
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok());
    let max = s.writes.limits.max_body;
    // A compressed body's size is not known before it is read; JSON numbers compress about 8:1.
    let bytes = match declared {
        Some(n) if encoded => n.saturating_mul(8).min(max),
        Some(n) => n.min(max),
        None => max,
    };
    let permits = s.writes.permits(bytes);
    match s.writes.budget.clone().try_acquire_many_owned(permits) {
        Ok(held) => {
            let res = next.run(req).await;
            drop(held);
            res
        }
        Err(_) => {
            let mut res = ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "the engine is busy writing other batches; retry after the Retry-After seconds",
            )
            .with_code(code::OVERLOADED)
            .into_response();
            res.headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
            res
        }
    }
}
