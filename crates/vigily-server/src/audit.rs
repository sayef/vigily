//! The audit trail of configuration changes: secrets, notification channels and routing rules.
//! Each entry names who made the change, what it was and what it touched, never a secret value.
//! Entries are records `audit/<at:020>-<random>` and are kept.
//!
//! | Method and path      | Scope | What it does |
//! |----------------------|-------|--------------|
//! | `GET /api/v1/audit`  | admin | Entries, newest first (`?limit=`, 100 by default, at most 1000) |

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use vigily_api::secrets::{AuditEntry, AuditLog, AuditQuery};
use vigily_core::{now_micros, RecordStore};

use crate::auth::Scoped;
use crate::{ApiError, AppState};

const PREFIX: &str = "audit/";

/// The most entries one request returns.
pub const MAX_LIMIT: usize = 1000;

/// Records a change. A failure to write is logged and does not undo the change.
pub async fn record(rs: &dyn RecordStore, principal: &str, action: &str, target: &str) {
    let entry = AuditEntry {
        at: now_micros(),
        principal: principal.to_string(),
        action: action.to_string(),
        target: target.to_string(),
    };
    tracing::info!(principal, action, target, "configuration changed");
    let key = format!(
        "{PREFIX}{:020}-{}",
        entry.at,
        &crate::auth::new_secret()[..8]
    );
    let written = match serde_json::to_value(&entry) {
        Ok(v) => match rs.put(vec![(key, v)]).await {
            Ok(()) => rs.flush().await,
            Err(e) => Err(e),
        },
        Err(e) => Err(vigily_core::Error::Storage(e.to_string())),
    };
    if let Err(e) = written {
        tracing::warn!(error = %e, action, target, "could not write an audit entry");
    }
}

/// Entries, newest first.
pub async fn list(rs: &dyn RecordStore, limit: usize) -> vigily_core::Result<Vec<AuditEntry>> {
    let rows = rs.scan(PREFIX).await?;
    Ok(rows
        .into_iter()
        .rev()
        .filter_map(|(_, v)| serde_json::from_value(v).ok())
        .take(limit)
        .collect())
}

/// The audit route, relative to the API root.
pub fn routes() -> Scoped {
    Scoped::default().admin(Router::new().route("/audit", get(audit)))
}

async fn audit(
    State(s): State<Arc<AppState>>,
    Query(q): Query<AuditQuery>,
) -> Result<Json<AuditLog>, ApiError> {
    let limit = q.limit.unwrap_or(100).clamp(1, MAX_LIMIT);
    Ok(Json(AuditLog {
        entries: list(&*s.records, limit).await?,
    }))
}
