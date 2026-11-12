//! The catalog's view of runs: series with their statistics, and the experiments, metric keys and
//! parameter keys of a repository.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::{Micros, Series, Summary};

/// One series and its statistics.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SeriesEntry {
    #[serde(flatten)]
    pub series: Series,
    pub key: String,
    pub summary: Summary,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ExperimentInfo {
    /// Empty for runs without an experiment.
    pub name: String,
    pub runs: usize,
    pub running: usize,
    pub finished: usize,
    pub failed: usize,
    pub aborted: usize,
    pub archived: usize,
    pub first_created: Option<Micros>,
    pub last_created: Option<Micros>,
    pub metrics: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MetricInfo {
    pub key: String,
    pub name: String,
    pub context: Value,
    pub runs: usize,
    /// How many of those runs are eval runs.
    pub eval_runs: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ParamInfo {
    pub key: String,
    /// number, text, bool, list, null or mixed.
    pub kind: String,
    pub runs: usize,
    /// Distinct values (counted up to 65).
    pub distinct: usize,
    /// The distinct values when there are at most 64, for grouping and autocompletion.
    pub values: Vec<Value>,
}
