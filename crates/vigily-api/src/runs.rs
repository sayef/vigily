//! Runs: rows of the runs table, a run's detail, the repository summary and run deletion.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::catalog::{ExperimentInfo, MetricInfo, ParamInfo, SeriesEntry};
use crate::model::{RunStatus, Summary};

/// What a purge removed (`vigily runs purge`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PurgeSummary {
    /// Deleted runs whose data was erased.
    pub runs: usize,
    /// Data files rewritten without deleted rows.
    pub fragments_rewritten: usize,
    /// Older versions removed.
    pub versions_removed: u64,
    /// Bytes of files removed.
    pub bytes_removed: u64,
    /// Tags deleted because they kept erased data (only with `force`).
    pub tags_deleted: Vec<String>,
}

/// A run as a row of the runs table.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunRow {
    pub id: String,
    /// The name, or the start of the id when the run has none.
    pub name: String,
    pub has_name: bool,
    pub experiment: Option<String>,
    pub description: Option<String>,
    pub tags: Vec<String>,
    pub status: RunStatus,
    /// `training`, `eval` or another kind.
    pub kind: String,
    /// The target reference (model, prompt, endpoint, git sha), or null.
    pub target: Value,
    pub archived: bool,
    pub created_at: i64,
    pub updated_at: i64,
    pub ended_at: Option<i64>,
    /// The last sign of life: a heartbeat, metadata write or point (microseconds, UTC).
    pub last_seen: i64,
    /// Running, but nothing heard for 10 minutes: the process most likely died.
    pub stale: bool,
    /// Seconds, up to now for running runs.
    pub duration: f64,
    pub params: BTreeMap<String, Value>,
    pub metrics: BTreeMap<String, MetricStat>,
}

/// The statistics of one series shown in the runs table.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MetricStat {
    pub last: Option<f64>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub count: u64,
    pub last_step: Option<i64>,
    pub min_step: Option<i64>,
    pub max_step: Option<i64>,
}

impl From<&Summary> for MetricStat {
    fn from(s: &Summary) -> Self {
        Self {
            last: s.last,
            min: s.min,
            max: s.max,
            count: s.count,
            last_step: s.last_step,
            min_step: s.min_step,
            max_step: s.max_step,
        }
    }
}

/// `GET meta`: the repository at a glance.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Meta {
    pub version: String,
    pub repository: String,
    pub runs: usize,
    pub running: usize,
    pub archived: usize,
    /// Runs per kind.
    pub kinds: BTreeMap<String, usize>,
    pub datasets: usize,
    pub schedules: usize,
    pub open_alerts: usize,
    pub traces: usize,
    pub refreshed_at: i64,
    pub experiments: Vec<ExperimentInfo>,
    pub metrics: Vec<MetricInfo>,
    pub params: Vec<ParamInfo>,
}

/// `GET runs`: the runs matching a filter, sorted.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunsResponse {
    /// Every run in the repository (not only the matching ones).
    pub total: usize,
    pub rows: Vec<RunRow>,
    pub refreshed_at: i64,
}

/// `GET runs/{id}`: one run with its parameter tree and series.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunDetail {
    #[serde(flatten)]
    pub row: RunRow,
    pub params_tree: Value,
    pub series: Vec<SeriesEntry>,
}

/// `POST runs/delete`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeleteRuns {
    pub ids: Vec<String>,
}

/// What `POST runs/delete` deleted.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Deleted {
    pub deleted: usize,
}
