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
    /// The records table of an earlier version (`records.lance`) is still there, already migrated.
    /// A purge does not reach it: it holds every record as of the upgrade until deleted by hand.
    #[serde(default)]
    pub legacy_records_present: bool,
}

/// A run without its parameters and metric statistics (`GET runs?fields=summary`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunSummary {
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
}

/// A run as a row of the runs table.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunRow {
    #[serde(flatten)]
    pub run: RunSummary,
    /// Flattened parameters (`hparams.lr`).
    pub params: BTreeMap<String, Value>,
    /// Statistics by metric key (`loss[val]`).
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

/// `GET runs`: the runs matching a filter, sorted, a page of them. Rows are [`RunRow`]s, or
/// [`RunSummary`]s with `fields=summary`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunsResponse<R = RunRow> {
    /// Every run in the repository (not only the matching ones).
    pub total: usize,
    /// The runs matching the filter, before `offset` and `limit`.
    pub matched: usize,
    pub rows: Vec<R>,
    pub refreshed_at: i64,
}

/// Which fields `GET runs` answers with.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunFields {
    /// Rows with flattened parameters and metric statistics.
    #[default]
    Full,
    /// Rows without them: smaller, for lists and the command line.
    Summary,
}

/// `GET runs/{id}/params`: a run's parameters as a nested object.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunParams {
    pub run_id: String,
    pub params: Value,
}

/// `GET runs/{id}/series`: a run's metric series with their statistics.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunSeries {
    pub run_id: String,
    pub series: Vec<SeriesEntry>,
}

/// `GET runs/{id}/points?name=&context=&start_time=&stop_time=`: the points of one series, in
/// step order. A value that is not finite is the string `NaN`, `Infinity` or `-Infinity`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SeriesPoints {
    pub run_id: String,
    pub name: String,
    pub context: Value,
    /// The metric key: `loss` or `loss[val]`.
    pub key: String,
    pub steps: Vec<i64>,
    #[serde(with = "non_finite")]
    pub values: Vec<f64>,
    pub epochs: Vec<Option<i64>>,
    /// UTC microseconds.
    pub timestamps: Vec<i64>,
}

/// Numbers as JSON numbers, and NaN and the infinities (which JSON lacks) as strings.
pub mod non_finite {
    use serde::de::Error;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use serde_json::Value;

    fn to_value(v: f64) -> Value {
        if v.is_finite() {
            serde_json::json!(v)
        } else if v.is_nan() {
            Value::from("NaN")
        } else if v > 0.0 {
            Value::from("Infinity")
        } else {
            Value::from("-Infinity")
        }
    }

    pub fn serialize<S: Serializer>(v: &[f64], s: S) -> Result<S::Ok, S::Error> {
        v.iter()
            .map(|x| to_value(*x))
            .collect::<Vec<_>>()
            .serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<f64>, D::Error> {
        Vec::<Value>::deserialize(d)?
            .into_iter()
            .map(|v| match &v {
                Value::Number(n) => n.as_f64().ok_or_else(|| D::Error::custom("not a number")),
                Value::String(s) => match s.as_str() {
                    "NaN" => Ok(f64::NAN),
                    "Infinity" => Ok(f64::INFINITY),
                    "-Infinity" => Ok(f64::NEG_INFINITY),
                    other => Err(D::Error::custom(format!("not a number: {other:?}"))),
                },
                other => Err(D::Error::custom(format!("not a number: {other}"))),
            })
            .collect()
    }
}

/// `POST admin/purge`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PurgeRequest {
    /// Also delete storage tags that keep erased data.
    #[serde(default)]
    pub force: bool,
}

/// `POST admin/optimize`: storage was compacted.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Optimized {
    pub seconds: f64,
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
