//! The evaluation API: datasets, per-item results and labels, run comparison, the audit trail,
//! schedules, suites and drift, alerts.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use axum::extract::{Path, Query as QueryParams, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use chrono::{TimeZone, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use vigily_api::evals::{
    AlertChange, CompareResponse, CompareRow, CompareRun, DataDriftResponse, DatasetResponse,
    DatasetRun, DriftDetail, HistoryPoint, ItemRow, ItemsResponse, LinkedRun, NewLabel, Schedule,
    ScheduleRunDetail, ScheduleRunListed, ScheduleRunStarted, ScheduleRunSummary,
    ScheduleRunsResponse, SchedulesResponse, ScoreSummary, Side, SuiteDetail, SuiteSummary,
    Thresholds, Verdict, VersionSummary,
};
use vigily_core::drift::{self, DriftConfig, Suite};
use vigily_core::evals::{self, Alert, Baseline, EvalResult, Item, Label, ScheduleRun};
use vigily_core::{now_micros, stats, RunEntry, RunStatus};

use crate::auth::Scoped;
use crate::{ApiError, AppState};

type ApiResult<T> = Result<Json<T>, ApiError>;

/// The evaluation routes, relative to the API root, by the scope they need.
pub fn routes() -> Scoped {
    Scoped::default()
        .read(
            Router::new()
                .route("/datasets", get(list_datasets))
                .route("/datasets/{name}", get(dataset))
                .route("/runs/{id}/items", get(run_items))
                .route("/runs/{id}/audit", get(audit))
                .route("/compare", get(compare))
                .route("/schedules", get(schedules))
                .route("/schedules/{name}/runs", get(schedule_runs))
                .route("/schedules/{name}/runs/{id}", get(schedule_run))
                .route("/suites", get(suites))
                .route("/suites/{key}", get(suite))
                .route("/suites/{key}/data-drift", get(data_drift))
                .route("/alerts", get(alerts)),
        )
        .admin(
            Router::new()
                .route("/runs/{id}/labels", post(add_label))
                .route("/suites/{key}/baseline", put(set_baseline))
                .route("/alerts/{id}", post(update_alert)),
        )
}

/// Evaluation routes added with `/api/v1`, which have no alias under `/api`.
pub fn v1_routes() -> Scoped {
    Scoped::default()
        .read(
            Router::new()
                .route("/datasets/{name}/versions", get(dataset_versions))
                .route(
                    "/datasets/{name}/versions/{version}/items",
                    get(dataset_items),
                )
                .route("/runs/{id}/results", get(run_results)),
        )
        .admin(Router::new().route("/schedules/{name}/run", post(run_schedule)))
}

fn bad(m: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, m)
}

fn not_found(m: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, m)
}

pub fn utc(us: i64) -> String {
    Utc.timestamp_micros(us)
        .single()
        .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_default()
}

/// What a suite's drift history depends on: its runs, the baseline and the thresholds. Finished
/// runs do not change, so the history is reused while this stays the same.
async fn fingerprint(
    s: &AppState,
    suite: &Suite,
    b: &Baseline,
    cfg: DriftConfig,
) -> Result<String, ApiError> {
    let last = suite.runs.last();
    // Results and features of the newest run may still be arriving after it is marked finished.
    let (mut results, mut features) = (0, 0);
    if let Some(r) = last {
        results = s.records.keys(&format!("res/{}/", r.run_id)).await?.len();
        features = s.records.keys(&format!("feat/{}/", r.run_id)).await?.len();
    }
    Ok(format!(
        "{}:{}:{:?}:{results}:{features}:{}:{}:{}",
        suite.runs.len(),
        last.map_or("", |r| r.run_id.as_str()),
        last.map(|r| r.status),
        b.updated_at,
        cfg.alpha,
        cfg.min_effect
    ))
}

type BehaviourHistory = Vec<(String, Option<drift::BehaviourDrift>)>;
type DataHistory = Vec<(String, Vec<String>, Vec<drift::FeatureDrift>)>;

async fn cached<T: Send + Sync + 'static>(
    s: &AppState,
    key: String,
    fp: String,
    compute: impl std::future::Future<Output = vigily_core::Result<T>>,
) -> Result<Arc<T>, ApiError> {
    if let Some((f, v)) = s.drift_cache.lock().get(&key) {
        if *f == fp {
            if let Ok(v) = v.clone().downcast::<T>() {
                return Ok(v);
            }
        }
    }
    let v = Arc::new(compute.await?);
    s.drift_cache.lock().insert(key, (fp, v.clone()));
    Ok(v)
}

async fn behaviour(
    s: &AppState,
    suite: &Suite,
    score: &str,
    b: &Baseline,
    cfg: DriftConfig,
    fp: &str,
) -> Result<Arc<BehaviourHistory>, ApiError> {
    let key = format!("{}/behaviour/{score}", suite.key);
    cached(
        s,
        key,
        fp.to_string(),
        drift::behaviour_history(&*s.records, suite, score, b, cfg),
    )
    .await
}

async fn data(
    s: &AppState,
    suite: &Suite,
    b: &Baseline,
    fp: &str,
) -> Result<Arc<DataHistory>, ApiError> {
    let key = format!("{}/data", suite.key);
    cached(
        s,
        key,
        fp.to_string(),
        drift::data_history(&*s.records, suite, b),
    )
    .await
}

fn drift_cfg(s: &AppState) -> DriftConfig {
    DriftConfig {
        alpha: s.config.drift.alpha,
        min_effect: s.config.drift.min_effect,
    }
}

// ---------------------------------------------------------------------------------------------
// Datasets

async fn list_datasets(State(s): State<Arc<AppState>>) -> ApiResult<Vec<evals::DatasetInfo>> {
    s.records.refresh().await?;
    let mut v = evals::datasets(&*s.records).await?;
    v.sort_by_key(|d| std::cmp::Reverse(d.updated_at));
    Ok(Json(v))
}

#[derive(Deserialize)]
struct DatasetParams {
    version: Option<u32>,
    #[serde(default)]
    q: String,
    #[serde(default)]
    offset: usize,
    limit: Option<usize>,
}

async fn dataset(
    State(s): State<Arc<AppState>>,
    Path(name): Path<String>,
    QueryParams(p): QueryParams<DatasetParams>,
) -> ApiResult<DatasetResponse> {
    s.records.refresh().await?;
    let info = evals::datasets(&*s.records)
        .await?
        .into_iter()
        .find(|d| d.name == name)
        .ok_or_else(|| not_found(format!("no dataset named {name}")))?;
    let versions = evals::dataset_versions(&*s.records, &name).await?;
    let v = evals::dataset_version(&*s.records, &name, p.version).await?;
    let items = evals::dataset_items(&*s.records, &v).await?;
    let q = p.q.trim().to_lowercase();
    let matching: Vec<Item> = items
        .into_iter()
        .filter(|i| {
            q.is_empty()
                || i.id.to_lowercase().contains(&q)
                || i.input.to_string().to_lowercase().contains(&q)
                || i.expected.to_string().to_lowercase().contains(&q)
        })
        .collect();
    let total = matching.len();
    let limit = p.limit.unwrap_or(500).min(5000);
    let page = matching.into_iter().skip(p.offset).take(limit).collect();
    let snap = s.catalog.snapshot();
    let runs = snap
        .runs
        .iter()
        .filter(|e| {
            e.params
                .get(drift::EVAL_META)
                .and_then(|m| m.get("dataset"))
                .and_then(|d| d.get("name"))
                .and_then(Value::as_str)
                == Some(&name)
        })
        .take(50)
        .map(|e| DatasetRun {
            id: e.run.id.clone(),
            name: e.run.display_name(),
            created_at: e.run.created_at,
            version: e.params[drift::EVAL_META]["dataset"]["version"].clone(),
            target: e.target(),
        })
        .collect();
    Ok(Json(DatasetResponse {
        info,
        version: (&v).into(),
        versions: versions.iter().rev().map(Into::into).collect(),
        total,
        items: page,
        runs,
    }))
}

async fn dataset_info(s: &AppState, name: &str) -> Result<evals::DatasetInfo, ApiError> {
    evals::datasets(&*s.records)
        .await?
        .into_iter()
        .find(|d| d.name == name)
        .ok_or_else(|| not_found(format!("no dataset named {name}")))
}

/// A dataset's versions, newest first, without their item lists.
async fn dataset_versions(
    State(s): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> ApiResult<Vec<VersionSummary>> {
    s.records.refresh().await?;
    dataset_info(&s, &name).await?;
    let versions = evals::dataset_versions(&*s.records, &name).await?;
    Ok(Json(versions.iter().rev().map(Into::into).collect()))
}

/// One version's items as newline-delimited JSON, one item per line, in the version's order.
async fn dataset_items(
    State(s): State<Arc<AppState>>,
    Path((name, version)): Path<(String, u32)>,
) -> Result<Response, ApiError> {
    s.records.refresh().await?;
    dataset_info(&s, &name).await?;
    let v = evals::dataset_version(&*s.records, &name, Some(version)).await?;
    let items = evals::dataset_items(&*s.records, &v).await?;
    // Serialised a chunk at a time as the body is sent, so a large version is never held twice.
    let chunks = items.chunks(256).map(<[Item]>::to_vec).collect::<Vec<_>>();
    let stream = futures_util::stream::iter(chunks.into_iter().map(|chunk| {
        let mut buf = Vec::new();
        for item in &chunk {
            serde_json::to_writer(&mut buf, item).map_err(std::io::Error::other)?;
            buf.push(b'\n');
        }
        Ok::<_, std::io::Error>(buf)
    }));
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/x-ndjson")],
        axum::body::Body::from_stream(stream),
    )
        .into_response())
}

// ---------------------------------------------------------------------------------------------
// Eval items, labels, comparison, audit

fn eval_entry(s: &AppState, id: &str) -> Result<Arc<RunEntry>, ApiError> {
    let snap = s.catalog.snapshot();
    let e = snap
        .get(id)
        .ok_or_else(|| not_found(format!("no run with id {id}")))?;
    Ok(e.clone())
}

async fn items_by_hash(s: &AppState, dataset: &str) -> Result<HashMap<String, Item>, ApiError> {
    Ok(s.records
        .scan(&format!("dsi/{dataset}/"))
        .await?
        .into_iter()
        .filter_map(|(_, v)| serde_json::from_value::<Item>(v).ok())
        .map(|i| (i.hash.clone(), i))
        .collect())
}

fn failed(r: &EvalResult) -> bool {
    r.error.is_some() || r.scores.iter().any(|s| s.passed == Some(false))
}

#[derive(Deserialize)]
struct ItemsParams {
    #[serde(default)]
    failures: bool,
}

async fn run_items(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    QueryParams(p): QueryParams<ItemsParams>,
) -> ApiResult<ItemsResponse> {
    let e = eval_entry(&s, &id)?;
    s.records.refresh().await?;
    let results = evals::results(&*s.records, &id).await?;
    let labels = evals::labels(&*s.records, &id).await?;
    let dataset = results.first().map(|r| r.dataset.clone());
    let items = match &dataset {
        Some(d) => items_by_hash(&s, d).await?,
        None => HashMap::new(),
    };
    let mut by_item: HashMap<String, Vec<Label>> = HashMap::new();
    for l in labels {
        by_item.entry(l.item_id.clone()).or_default().push(l);
    }
    let mut scores: BTreeMap<String, (f64, usize, usize, usize)> = BTreeMap::new();
    for r in &results {
        for sc in &r.scores {
            let x = scores.entry(sc.name.clone()).or_default();
            if let Some(v) = sc.value.or(sc.passed.map(|b| b as u8 as f64)) {
                x.0 += v;
                x.1 += 1;
            }
            if let Some(b) = sc.passed {
                x.2 += b as usize;
                x.3 += 1;
            }
        }
    }
    let total = results.len();
    let n_failed = results.iter().filter(|r| failed(r)).count();
    let errors = results.iter().filter(|r| r.error.is_some()).count();
    // Traces of this run by evaluation item, for results written without a trace id.
    let mut traced: HashMap<String, String> =
        vigily_core::traces::list_for_runs(&*s.records, &[id.as_str()])
            .await
            .unwrap_or_default()
            .into_iter()
            .filter_map(|t| t.eval_item.map(|i| (i, t.id)))
            .collect();
    let rows = results
        .into_iter()
        .filter(|r| !p.failures || failed(r))
        .map(|r| {
            let it = items.get(&r.item_hash);
            let item_key = r.item_id.clone();
            ItemRow {
                failed: failed(&r),
                input: it.map_or(Value::Null, |i| i.input.clone()),
                expected: it.map_or(Value::Null, |i| i.expected.clone()),
                metadata: it.map_or(Value::Null, |i| i.metadata.clone()),
                labels: by_item.remove(&r.item_id).unwrap_or_default(),
                item_id: r.item_id,
                output: r.output,
                error: r.error,
                latency_ms: r.latency_ms,
                scores: r.scores,
                trace_id: r.trace_id.or_else(|| traced.remove(&item_key)),
            }
        })
        .collect();
    Ok(Json(ItemsResponse {
        run_id: id,
        dataset: e
            .params
            .get(drift::EVAL_META)
            .and_then(|m| m.get("dataset"))
            .cloned(),
        total,
        failed: n_failed,
        errors,
        scores: scores
            .into_iter()
            .map(|(k, (sum, n, pass, np))| {
                (
                    k,
                    ScoreSummary {
                        mean: (n > 0).then(|| sum / n as f64),
                        pass_rate: (np > 0).then(|| pass as f64 / np as f64),
                        n,
                    },
                )
            })
            .collect(),
        items: rows,
    }))
}

/// The per-item results of an eval run, as the SDK stored them.
async fn run_results(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> ApiResult<Vec<EvalResult>> {
    eval_entry(&s, &id)?;
    s.records.refresh().await?;
    Ok(Json(evals::results(&*s.records, &id).await?))
}

async fn add_label(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    by: Option<axum::Extension<crate::auth::AuthContext>>,
    Json(b): Json<NewLabel>,
) -> ApiResult<Label> {
    eval_entry(&s, &id)?;
    let name = b.name.trim();
    if name.is_empty() || name.len() > 64 {
        return Err(bad(
            "a label needs a name of 1 to 64 characters, such as human or correct",
        ));
    }
    if b.value.is_none()
        && b.passed.is_none()
        && b.comment.as_deref().is_none_or(|c| c.trim().is_empty())
    {
        return Err(bad("a label needs a value, a pass or fail, or a comment"));
    }
    if s.records
        .get(&format!("res/{id}/{}", b.item_id))
        .await?
        .is_none()
    {
        return Err(not_found(format!(
            "run {id} has no result for item {}",
            b.item_id
        )));
    }
    let l = evals::put_label(
        &*s.records,
        Label {
            run_id: id,
            item_id: b.item_id,
            name: name.to_string(),
            value: b.value.filter(|v| v.is_finite()),
            passed: b.passed,
            comment: b
                .comment
                .map(|c| c.trim().chars().take(4000).collect())
                .filter(|c: &String| !c.is_empty()),
            created_at: 0,
            by: by.map(|c| c.principal.clone()),
        },
    )
    .await?;
    Ok(Json(l))
}

#[derive(Deserialize)]
struct CompareParams {
    a: String,
    b: String,
}

async fn compare(
    State(s): State<Arc<AppState>>,
    QueryParams(p): QueryParams<CompareParams>,
) -> ApiResult<CompareResponse> {
    let (ea, eb) = (eval_entry(&s, &p.a)?, eval_entry(&s, &p.b)?);
    s.records.refresh().await?;
    let ra = evals::results(&*s.records, &p.a).await?;
    let rb = evals::results(&*s.records, &p.b).await?;
    let mut items = HashMap::new();
    for d in ra
        .iter()
        .chain(&rb)
        .map(|r| r.dataset.clone())
        .collect::<std::collections::BTreeSet<_>>()
    {
        items.extend(items_by_hash(&s, &d).await?);
    }
    let side = |r: &EvalResult| Side {
        output: r.output.clone(),
        error: r.error.clone(),
        scores: r.scores.clone(),
        failed: failed(r),
    };
    let mb: HashMap<&str, &EvalResult> = rb.iter().map(|r| (r.item_id.as_str(), r)).collect();
    let mut rows: Vec<CompareRow> = Vec::new();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut seen = std::collections::HashSet::new();
    for r in &ra {
        seen.insert(r.item_id.clone());
        let other = mb.get(r.item_id.as_str());
        let change = match other {
            None => "only_a",
            Some(o) => match (failed(r), failed(o)) {
                (false, true) => "regressed",
                (true, false) => "fixed",
                (true, true) => "both_failed",
                (false, false) if r.output != o.output => "changed",
                _ => "both_passed",
            },
        };
        *counts.entry(change.to_string()).or_default() += 1;
        let it = items.get(&r.item_hash);
        rows.push(CompareRow {
            item_id: r.item_id.clone(),
            input: it.map_or(Value::Null, |i| i.input.clone()),
            expected: it.map_or(Value::Null, |i| i.expected.clone()),
            a: Some(side(r)),
            b: other.map(|o| side(o)),
            change: change.to_string(),
        });
    }
    for o in rb.iter().filter(|o| !seen.contains(&o.item_id)) {
        *counts.entry("only_b".to_string()).or_default() += 1;
        let it = items.get(&o.item_hash);
        rows.push(CompareRow {
            item_id: o.item_id.clone(),
            input: it.map_or(Value::Null, |i| i.input.clone()),
            expected: it.map_or(Value::Null, |i| i.expected.clone()),
            a: None,
            b: Some(side(o)),
            change: "only_b".to_string(),
        });
    }
    let order = |c: &str| {
        [
            "regressed",
            "fixed",
            "changed",
            "both_failed",
            "only_a",
            "only_b",
            "both_passed",
        ]
        .iter()
        .position(|x| *x == c)
        .unwrap_or(9)
    };
    rows.sort_by(|x, y| {
        order(&x.change)
            .cmp(&order(&y.change))
            .then(x.item_id.cmp(&y.item_id))
    });
    let meta = |e: &RunEntry| CompareRun {
        id: e.run.id.clone(),
        name: e.run.display_name(),
        created_at: e.run.created_at,
        target: e.target(),
        dataset: e
            .params
            .get(drift::EVAL_META)
            .and_then(|m| m.get("dataset"))
            .cloned(),
    };
    Ok(Json(CompareResponse {
        a: meta(&ea),
        b: meta(&eb),
        counts,
        items: rows,
    }))
}

#[derive(Deserialize)]
struct AuditParams {
    #[serde(default)]
    download: bool,
}

/// Everything an eval result depends on, as one JSON document.
async fn audit(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    QueryParams(p): QueryParams<AuditParams>,
) -> Result<Response, ApiError> {
    let e = eval_entry(&s, &id)?;
    s.records.refresh().await?;
    let meta = e
        .params
        .get(drift::EVAL_META)
        .cloned()
        .unwrap_or(Value::Null);
    let dataset = match meta.get("dataset").and_then(|d| {
        Some((
            d.get("name")?.as_str()?.to_string(),
            d.get("version")?.as_u64()? as u32,
        ))
    }) {
        Some((name, version)) => {
            match evals::dataset_version(&*s.records, &name, Some(version)).await {
                Ok(v) => json!({
                    "name": v.name, "version": v.version, "digest": v.digest,
                    "created_at": utc(v.created_at), "items": v.items.len(), "source": v.source,
                    "note": v.note, "storage_version": v.storage_version, "storage_tag": v.storage_tag,
                }),
                Err(_) => json!({ "name": name, "version": version, "missing": true }),
            }
        }
        None => Value::Null,
    };
    let schedule = e
        .params
        .get(vigily_core::RUN_META)
        .and_then(|m| m.get("schedule"))
        .cloned();
    let schedule_run = match schedule.as_ref().and_then(|v| Some((v.get("name")?.as_str()?.to_string(), v.get("run")?.as_str()?.to_string()))) {
        Some((name, rid)) => s
            .records
            .get(&format!("sch/{name}/{rid}"))
            .await?
            .and_then(|v| serde_json::from_value::<ScheduleRun>(v).ok())
            .map(|r| {
                json!({
                    "id": r.id, "schedule": r.schedule, "scheduled_for": utc(r.scheduled_for),
                    "started_at": utc(r.started_at), "ended_at": r.ended_at.map(utc),
                    "attempt": r.attempt, "status": r.status, "exit_code": r.exit_code, "command": r.command,
                })
            }),
        None => None,
    };
    let results: Vec<Value> = evals::results(&*s.records, &id)
        .await?
        .into_iter()
        .map(|r| {
            json!({
                "item_id": r.item_id, "item_hash": r.item_hash, "dataset": r.dataset,
                "dataset_version": r.dataset_version, "output": r.output, "error": r.error,
                "latency_ms": r.latency_ms, "scores": r.scores, "recorded_at": utc(r.created_at),
            })
        })
        .collect();
    let labels: Vec<Value> = evals::labels(&*s.records, &id)
        .await?
        .into_iter()
        .map(|l| json!({"item_id": l.item_id, "name": l.name, "value": l.value, "passed": l.passed, "comment": l.comment, "recorded_at": utc(l.created_at), "source": "vigily web app", "by": l.by}))
        .collect();
    let r = &e.run;
    let doc = json!({
        "format": "vigily.audit/1",
        "exported_at": utc(now_micros()),
        "vigily": s.version,
        "repository": s.catalog.store().location(),
        "run": {
            "id": r.id, "name": r.name, "experiment": r.experiment, "kind": e.kind(),
            "status": r.status, "created_at": utc(r.created_at), "ended_at": r.ended_at.map(utc),
            "tags": r.tags, "notes": r.description,
        },
        "target": e.target(),
        "dataset": dataset,
        "evaluators": meta.get("evaluators").cloned().unwrap_or(Value::Null),
        "schedule": schedule,
        "schedule_run": schedule_run,
        "environment": e.params.get("__system__").cloned().unwrap_or(Value::Null),
        "results": results,
        "labels": labels,
    });
    let body = serde_json::to_string_pretty(&doc).unwrap_or_default();
    let mut headers = vec![(header::CONTENT_TYPE, "application/json".to_string())];
    if p.download {
        headers.push((
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"vigily-audit-{id}.json\""),
        ));
    }
    let mut resp = (StatusCode::OK, body).into_response();
    for (k, v) in headers {
        resp.headers_mut()
            .insert(k, v.parse().expect("valid header"));
    }
    Ok(resp)
}

// ---------------------------------------------------------------------------------------------
// Schedules

fn summary(r: &ScheduleRun, with_output: &std::collections::HashSet<String>) -> ScheduleRunSummary {
    ScheduleRunSummary::of(r, with_output.contains(&r.id))
}

/// Eval runs a schedule run logged, by schedule-run id.
fn linked_runs(s: &AppState, schedule: &str) -> HashMap<String, Vec<LinkedRun>> {
    let snap = s.catalog.snapshot();
    let mut out: HashMap<String, Vec<LinkedRun>> = HashMap::new();
    for e in &snap.runs {
        let Some(sch) = e
            .params
            .get(vigily_core::RUN_META)
            .and_then(|m| m.get("schedule"))
        else {
            continue;
        };
        if sch.get("name").and_then(Value::as_str) != Some(schedule) {
            continue;
        }
        let Some(rid) = sch.get("run").and_then(Value::as_str) else {
            continue;
        };
        out.entry(rid.to_string()).or_default().push(LinkedRun {
            id: e.run.id.clone(),
            name: e.run.display_name(),
            status: e.run.status,
            target: e.target(),
        });
    }
    out
}

async fn schedules(State(s): State<Arc<AppState>>) -> ApiResult<SchedulesResponse> {
    s.records.refresh().await?;
    let next = s.schedule_status.read().clone();
    let mut list = Vec::new();
    for c in &s.config.schedule {
        let runs = evals::schedule_runs(&*s.records, &c.name).await?;
        let out = evals::schedule_runs_with_output(&*s.records, &c.name).await?;
        let recent = runs.iter().take(14).map(|r| summary(r, &out)).collect();
        let workdir = std::fs::canonicalize(s.config.workdir(c))
            .unwrap_or_else(|_| s.config.workdir(c))
            .display()
            .to_string();
        list.push(Schedule {
            name: c.name.clone(),
            description: c.description.clone(),
            cron: c.cron.clone(),
            timezone: c.timezone.clone(),
            command: c.command.clone(),
            workdir,
            timeout: c.timeout.clone(),
            retries: c.retries,
            missed: c.missed,
            enabled: c.enabled,
            next_run: next.get(&c.name).copied(),
            last_run: runs.first().map(|r| summary(r, &out)),
            recent,
            runs: runs.len(),
        });
    }
    Ok(Json(SchedulesResponse {
        config_path: s.config.path.as_ref().map(|p| p.display().to_string()),
        schedules: list,
    }))
}

async fn schedule_runs(
    State(s): State<Arc<AppState>>,
    Path(name): Path<String>,
) -> ApiResult<ScheduleRunsResponse> {
    if !s.config.schedule.iter().any(|c| c.name == name) {
        return Err(not_found(format!(
            "no schedule named {name} in the configuration"
        )));
    }
    s.records.refresh().await?;
    let linked = linked_runs(&s, &name);
    let out = evals::schedule_runs_with_output(&*s.records, &name).await?;
    let runs = evals::schedule_runs(&*s.records, &name)
        .await?
        .iter()
        .take(500)
        .map(|r| ScheduleRunListed {
            run: summary(r, &out),
            eval_runs: linked.get(&r.id).cloned().unwrap_or_default(),
        })
        .collect();
    Ok(Json(ScheduleRunsResponse { runs }))
}

async fn schedule_run(
    State(s): State<Arc<AppState>>,
    Path((name, id)): Path<(String, String)>,
) -> ApiResult<ScheduleRunDetail> {
    s.records.refresh().await?;
    let mut r: ScheduleRun = s
        .records
        .get(&format!("sch/{name}/{id}"))
        .await?
        .and_then(|v| serde_json::from_value(v).ok())
        .ok_or_else(|| not_found(format!("no run {id} of schedule {name}")))?;
    let (stdout, stderr) = evals::schedule_run_output(&*s.records, &name, &id).await?;
    (r.stdout, r.stderr) = (stdout, stderr);
    let eval_runs = linked_runs(&s, &name).remove(&r.id).unwrap_or_default();
    Ok(Json(ScheduleRunDetail { run: r, eval_runs }))
}

/// Starts a schedule's command now, as the operator, and records it like a scheduled run (with
/// its retries). Answers at once with the first attempt's id; the run is followed with
/// `GET schedules/{name}/runs/{id}`. Refused while a run of the schedule is going.
async fn run_schedule(
    State(s): State<Arc<AppState>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    bytes: axum::body::Bytes,
) -> Result<Response, ApiError> {
    crate::from_a_client(&headers, "schedule runs")?;
    let _: Option<Value> = crate::admin_api::body(&bytes)?;
    let cfg = s
        .config
        .schedule
        .iter()
        .find(|c| c.name == name)
        .cloned()
        .ok_or_else(|| not_found(format!("no schedule named {name} in the configuration")))?;
    s.records.refresh().await?;
    if let Some(r) = evals::schedule_runs(&*s.records, &name)
        .await?
        .into_iter()
        .find(|r| r.status == evals::ScheduleRunStatus::Running)
    {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            format!(
                "schedule {name} is running now (run {}); start it again once that run ends",
                r.id
            ),
        ));
    }
    let first = crate::scheduler::start_now(s.clone(), cfg).await;
    let location = format!("/api/v1/schedules/{name}/runs/{}", first.id);
    let mut res = (
        StatusCode::ACCEPTED,
        Json(ScheduleRunStarted {
            schedule: name,
            id: first.id,
            status: first.status,
        }),
    )
        .into_response();
    if let Ok(v) = location.parse() {
        res.headers_mut().insert(header::LOCATION, v);
    }
    Ok(res)
}

// ---------------------------------------------------------------------------------------------
// Suites and drift

fn find_suite(s: &AppState, key: &str) -> Result<Suite, ApiError> {
    drift::suites(&s.catalog.snapshot())
        .into_iter()
        .find(|x| x.key == key)
        .ok_or_else(|| not_found(format!("no evaluation suite {key}")))
}

async fn suites(State(s): State<Arc<AppState>>) -> ApiResult<Vec<SuiteSummary>> {
    s.records.refresh().await?;
    let cfg = drift_cfg(&s);
    let mut out = Vec::new();
    for suite in drift::suites(&s.catalog.snapshot()) {
        let baseline = evals::baseline(&*s.records, &suite.key).await?;
        let latest = suite
            .runs
            .iter()
            .rev()
            .find(|r| r.status == RunStatus::Finished);
        let mut verdicts = Vec::new();
        let mut data_level = None;
        if let Some(l) = latest {
            // One fingerprint per suite: it reads the newest run's result and feature counts.
            let fp = fingerprint(&s, &suite, &baseline, cfg).await?;
            for score in &suite.scores {
                let h = behaviour(&s, &suite, score, &baseline, cfg, &fp).await?;
                let found = h
                    .iter()
                    .find(|(r, _)| *r == l.run_id)
                    .and_then(|(_, d)| d.as_ref());
                if let Some(d) = found {
                    verdicts.push(Verdict {
                        score: score.clone(),
                        drifted: d.drifted,
                        delta: d.delta,
                        p: d.test.as_ref().map(|t| t.p),
                        regressions: d.regressions.len(),
                    });
                }
            }
            let dh = data(&s, &suite, &baseline, &fp).await?;
            let dd: &[drift::FeatureDrift] = dh
                .iter()
                .find(|(r, _, _)| *r == l.run_id)
                .map_or(&[], |(_, _, f)| f.as_slice());
            data_level = dd.iter().map(|f| f.level).max_by_key(|l| *l as u8);
        }
        out.push(SuiteSummary {
            label: crate::monitor::suite_label(&suite),
            runs: suite.runs.len(),
            latest: latest.cloned(),
            key: suite.key,
            experiment: suite.experiment,
            target: suite.target,
            dataset: suite.dataset,
            schedule: suite.schedule,
            scores: suite.scores,
            baseline,
            behaviour: verdicts,
            data: data_level,
        });
    }
    Ok(Json(out))
}

#[derive(Deserialize)]
struct SuiteParams {
    run: Option<String>,
}

async fn suite(
    State(s): State<Arc<AppState>>,
    Path(key): Path<String>,
    QueryParams(p): QueryParams<SuiteParams>,
) -> ApiResult<SuiteDetail> {
    s.records.refresh().await?;
    let suite = find_suite(&s, &key)?;
    let baseline = evals::baseline(&*s.records, &key).await?;
    let cfg = drift_cfg(&s);
    let finished: Vec<&drift::SuiteRun> = suite
        .runs
        .iter()
        .filter(|r| r.status == RunStatus::Finished)
        .collect();
    let selected = p
        .run
        .clone()
        .or_else(|| finished.last().map(|r| r.run_id.clone()));
    let mut history: BTreeMap<String, Vec<HistoryPoint>> = BTreeMap::new();
    let mut detail = Vec::new();
    let items = match suite.dataset.as_deref() {
        Some(d) => items_by_hash(&s, d).await?,
        None => HashMap::new(),
    };
    let selected_hashes: HashMap<String, String> = match &selected {
        Some(run) => evals::results(&*s.records, run)
            .await?
            .into_iter()
            .map(|r| (r.item_id, r.item_hash))
            .collect(),
        None => HashMap::new(),
    };
    let fp = fingerprint(&s, &suite, &baseline, cfg).await?;
    for score in &suite.scores {
        let verdicts = behaviour(&s, &suite, score, &baseline, cfg, &fp).await?;
        let drifted: std::collections::HashSet<String> = verdicts
            .iter()
            .filter(|(_, d)| d.as_ref().is_some_and(|d| d.drifted))
            .map(|(r, _)| r.clone())
            .collect();
        let by_run: HashMap<&str, Option<&drift::BehaviourDrift>> = verdicts
            .iter()
            .map(|(r, d)| (r.as_str(), d.as_ref()))
            .collect();
        let mut points = Vec::new();
        for r in finished.iter().rev().take(120).rev() {
            // The band is the spread of the baseline runs' aggregate scores.
            let base: Vec<f64> = drift::baseline_runs(&suite, &r.run_id, &baseline, &drifted)
                .iter()
                .filter_map(|b| b.scores.get(score).copied())
                .collect();
            let d = by_run.get(r.run_id.as_str()).copied().flatten();
            let (m, sd) = (stats::mean(&base), stats::std_dev(&base));
            points.push(HistoryPoint {
                run_id: r.run_id.clone(),
                created_at: r.created_at,
                value: r.scores.get(score).copied(),
                baseline_mean: (!base.is_empty()).then_some(m),
                baseline_low: (!base.is_empty()).then_some(m - 2.0 * sd),
                baseline_high: (!base.is_empty()).then_some(m + 2.0 * sd),
                drifted: d.is_some_and(|d| d.drifted),
                p: d.and_then(|d| d.test.as_ref().map(|t| t.p)),
                regressions: d.map_or(0, |d| d.regressions.len()),
            });
        }
        history.insert(score.clone(), points);
        if let Some(d) = selected
            .as_deref()
            .and_then(|run| by_run.get(run).copied().flatten())
        {
            let regression_inputs = d
                .regressions
                .iter()
                .take(200)
                .map(|f| {
                    selected_hashes
                        .get(&f.item_id)
                        .and_then(|h| items.get(h))
                        .map_or(Value::Null, |i| i.input.clone())
                })
                .collect();
            detail.push(DriftDetail {
                drift: d.clone(),
                regression_inputs,
            });
        }
    }
    Ok(Json(SuiteDetail {
        label: crate::monitor::suite_label(&suite),
        key: suite.key,
        experiment: suite.experiment,
        target: suite.target,
        dataset: suite.dataset,
        schedule: suite.schedule,
        scores: suite.scores,
        runs: suite.runs,
        baseline,
        config: cfg,
        history,
        selected,
        drift: detail,
    }))
}

async fn set_baseline(
    State(s): State<Arc<AppState>>,
    Path(key): Path<String>,
    Json(b): Json<Baseline>,
) -> ApiResult<Baseline> {
    let suite = find_suite(&s, &key)?;
    if let Some(r) = b
        .runs
        .iter()
        .find(|r| !suite.runs.iter().any(|x| &x.run_id == *r))
    {
        return Err(bad(format!("run {r} is not part of this suite")));
    }
    Ok(Json(evals::put_baseline(&*s.records, &key, b).await?))
}

async fn data_drift(
    State(s): State<Arc<AppState>>,
    Path(key): Path<String>,
    QueryParams(p): QueryParams<SuiteParams>,
) -> ApiResult<DataDriftResponse> {
    s.records.refresh().await?;
    let suite = find_suite(&s, &key)?;
    let baseline = evals::baseline(&*s.records, &key).await?;
    let run = p
        .run
        .or_else(|| {
            suite
                .runs
                .iter()
                .rev()
                .find(|r| r.status == RunStatus::Finished)
                .map(|r| r.run_id.clone())
        })
        .ok_or_else(|| not_found("this suite has no finished run yet"))?;
    let cfg = drift_cfg(&s);
    let fp = fingerprint(&s, &suite, &baseline, cfg).await?;
    let history = data(&s, &suite, &baseline, &fp).await?;
    let (base, features) = history
        .iter()
        .find(|(r, _, _)| *r == run)
        .map(|(_, b, f)| (b.clone(), f.clone()))
        .unwrap_or_default();
    Ok(Json(DataDriftResponse {
        run_id: run,
        baseline_runs: base,
        features,
        thresholds: Thresholds {
            psi_moderate: drift::PSI_MODERATE,
            psi_drift: drift::PSI_DRIFT,
            js_moderate: drift::JS_MODERATE,
            js_drift: drift::JS_DRIFT,
        },
    }))
}

// ---------------------------------------------------------------------------------------------
// Alerts

async fn alerts(State(s): State<Arc<AppState>>) -> ApiResult<Vec<Alert>> {
    s.records.refresh().await?;
    Ok(Json(evals::alerts(&*s.records).await?))
}

async fn update_alert(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(b): Json<AlertChange>,
) -> ApiResult<Alert> {
    let mut a: Alert = s
        .records
        .get(&format!("alert/{id}"))
        .await?
        .and_then(|v| serde_json::from_value(v).ok())
        .ok_or_else(|| not_found(format!("no alert {id}")))?;
    a.acknowledged_at = b.acknowledged.then(now_micros);
    evals::update_alert(&*s.records, &a).await?;
    Ok(Json(a))
}
