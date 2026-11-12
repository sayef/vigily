//! Storage administration: erasing deleted data and compacting storage. Both refuse requests a web
//! page could send (see [`crate::from_a_client`]).
//!
//! | Method and path             | What it does |
//! |-----------------------------|--------------|
//! | `POST /api/v1/admin/purge`  | Erases deleted runs and traces from storage (`{"force": bool}`) |
//! | `POST /api/v1/admin/optimize` | Compacts storage |

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use serde::de::DeserializeOwned;
use vigily_api::code;
use vigily_api::runs::{Optimized, PurgeRequest, PurgeSummary};

use crate::{ApiError, AppState};

/// The admin routes, relative to the API root.
pub fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/admin/purge", post(purge))
        .route("/admin/optimize", post(optimize))
}

/// Reads a JSON body that may be empty (then every field takes its default).
pub(crate) fn body<T: DeserializeOwned + Default>(bytes: &Bytes) -> Result<T, ApiError> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(T::default());
    }
    serde_json::from_slice(bytes).map_err(|e| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            format!("the request body is not valid JSON for this route: {e}"),
        )
    })
}

/// The explanation that follows a purge refused because of a storage tag.
pub fn tag_blocks_purge_message(error: &str) -> String {
    format!(
        "{error}\nA storage tag keeps an older version that still holds deleted data, so nothing was purged. \
         Run `vigily runs purge --force` to delete such tags and purge, after checking that nothing needs that tag. \
         Tags on evaluation datasets never block a purge: they keep only their dataset's table."
    )
}

/// Purges storage, mapping a refusal because of a tag to `409 tag_blocks_purge`.
pub(crate) async fn run_purge(s: &AppState, force: bool) -> Result<PurgeSummary, ApiError> {
    let mut summary = match s.catalog.store().purge(force).await {
        Ok(r) => r,
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("tag") && !force {
                return Err(
                    ApiError::new(StatusCode::CONFLICT, tag_blocks_purge_message(&msg))
                        .with_code(code::TAG_BLOCKS_PURGE),
                );
            }
            return Err(e.into());
        }
    };
    summary.legacy_records_present = s.records.legacy_present().await?;
    // Deleted traces and runs are gone from storage now.
    *s.traces_count.lock() = None;
    *s.traces_cache.lock() = None;
    s.catalog.refresh().await?;
    tracing::info!(
        runs = summary.runs,
        bytes = summary.bytes_removed,
        "purged storage"
    );
    Ok(summary)
}

async fn purge(
    State(s): State<Arc<AppState>>,
    headers: HeaderMap,
    bytes: Bytes,
) -> Result<Json<PurgeSummary>, ApiError> {
    crate::from_a_client(&headers, "admin requests")?;
    let req: PurgeRequest = body(&bytes)?;
    Ok(Json(run_purge(&s, req.force).await?))
}

async fn optimize(
    State(s): State<Arc<AppState>>,
    headers: HeaderMap,
    bytes: Bytes,
) -> Result<Json<Optimized>, ApiError> {
    crate::from_a_client(&headers, "admin requests")?;
    let _: serde_json::Value = body::<Option<serde_json::Value>>(&bytes)?.unwrap_or_default();
    let started = std::time::Instant::now();
    s.catalog.store().optimize().await?;
    // On a large heap trimming takes a while: keep it off the request-serving threads.
    let _ = tokio::task::spawn_blocking(crate::release_memory).await;
    s.catalog.refresh().await?;
    Ok(Json(Optimized {
        seconds: started.elapsed().as_secs_f64(),
    }))
}
