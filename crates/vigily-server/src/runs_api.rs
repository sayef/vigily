//! A run's data beyond its row: parameters, series and the points of one series.
//!
//! | Method and path                      | What it returns |
//! |--------------------------------------|-----------------|
//! | `GET /api/v1/runs/{id}/params`       | The parameters as a nested object |
//! | `GET /api/v1/runs/{id}/series`       | The metric series with their statistics |
//! | `GET /api/v1/runs/{id}/points?name=&context=&start_time=&stop_time=` | One series' points |

use std::sync::Arc;

use axum::extract::{Path, Query as QueryParams, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::Value;
use vigily_api::runs::{RunParams, RunSeries, SeriesPoints};
use vigily_core::{metric_key, RunEntry, StepRange, TimeRange};

use crate::{ApiError, AppState};

type ApiResult<T> = Result<Json<T>, ApiError>;

/// The run-data routes, relative to the API root.
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/runs/{id}/params", get(params))
        .route("/runs/{id}/series", get(series))
        .route("/runs/{id}/points", get(points))
}

async fn entry(s: &AppState, id: &str) -> Result<Arc<RunEntry>, ApiError> {
    let snap = s.catalog.refresh_if_older(1_000_000).await?;
    snap.get(id)
        .cloned()
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, format!("no run with id {id}")))
}

async fn params(State(s): State<Arc<AppState>>, Path(id): Path<String>) -> ApiResult<RunParams> {
    let e = entry(&s, &id).await?;
    Ok(Json(RunParams {
        run_id: id,
        params: e.params.clone(),
    }))
}

async fn series(State(s): State<Arc<AppState>>, Path(id): Path<String>) -> ApiResult<RunSeries> {
    let e = entry(&s, &id).await?;
    Ok(Json(RunSeries {
        run_id: id,
        series: e.series.clone(),
    }))
}

#[derive(Deserialize)]
struct PointsParams {
    /// The metric name.
    name: String,
    /// The context as a JSON object (`{"subset":"val"}`); none for a series without context.
    context: Option<String>,
    /// UTC microseconds: points at or after.
    start_time: Option<i64>,
    /// UTC microseconds: points before.
    stop_time: Option<i64>,
}

async fn points(
    State(s): State<Arc<AppState>>,
    Path(id): Path<String>,
    QueryParams(p): QueryParams<PointsParams>,
) -> ApiResult<SeriesPoints> {
    let context: Value = match p.context.as_deref().map(str::trim) {
        None | Some("") => Value::Object(Default::default()),
        Some(c) => match serde_json::from_str(c) {
            Ok(v @ Value::Object(_)) => v,
            _ => {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    format!(
                        "context must be a JSON object such as {{\"subset\":\"val\"}}, not {c}"
                    ),
                ))
            }
        },
    };
    let e = entry(&s, &id).await?;
    let key = metric_key(&p.name, &context);
    let found = e.series_by_key(&key).ok_or_else(|| {
        ApiError::new(
            StatusCode::NOT_FOUND,
            format!("run {id} has no series {key}"),
        )
    })?;
    let time = TimeRange {
        start: p.start_time,
        stop: p.stop_time,
    };
    let sid = found.series.id;
    let pts = s
        .catalog
        .store()
        .points_in_time(&[sid], StepRange::default(), time)
        .await?
        .remove(&sid)
        .unwrap_or_default();
    Ok(Json(SeriesPoints {
        run_id: id,
        name: found.series.name.clone(),
        context: found.series.context.clone(),
        key,
        steps: pts.steps,
        values: pts.values,
        epochs: pts.epochs,
        timestamps: pts.timestamps,
    }))
}
