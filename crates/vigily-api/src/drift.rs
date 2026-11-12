//! Suites and drift: the eval runs that measure the same thing, and the behaviour- and data-drift
//! verdicts computed over them.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::RunStatus;

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct DriftConfig {
    pub alpha: f64,
    pub min_effect: f64,
}

impl Default for DriftConfig {
    fn default() -> Self {
        Self {
            alpha: 0.01,
            min_effect: 0.02,
        }
    }
}

/// One run of a suite.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SuiteRun {
    pub run_id: String,
    pub name: String,
    pub created_at: i64,
    pub status: RunStatus,
    pub dataset_version: Option<u64>,
    pub schedule: Option<String>,
    /// Aggregate scores by score name.
    pub scores: BTreeMap<String, f64>,
}

/// Eval runs that measure the same thing.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Suite {
    pub key: String,
    pub experiment: Option<String>,
    pub target: Value,
    pub dataset: Option<String>,
    pub schedule: Option<String>,
    /// Oldest first.
    pub runs: Vec<SuiteRun>,
    pub scores: Vec<String>,
}

/// An item whose verdict changed between the previous run and this one.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Flip {
    pub item_id: String,
    pub before: Option<f64>,
    pub after: Option<f64>,
}

/// A test statistic and its p-value.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TestResult {
    /// `wilcoxon signed-rank` (paired) or `mann-whitney u` (unpaired fallback).
    pub name: String,
    pub statistic: f64,
    pub z: f64,
    pub p: f64,
}

/// The behaviour-drift verdict for one score of one run.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BehaviourDrift {
    pub score: String,
    pub run_id: String,
    pub baseline_runs: Vec<String>,
    pub higher_is_better: bool,
    /// True when items were matched and tested in pairs; false for the unpaired fallback.
    pub paired: bool,
    /// Items compared: matched pairs when paired, else the run's items.
    pub n_current: usize,
    /// Baseline values compared: the same matched items when paired, else per-item means.
    pub n_baseline: usize,
    pub current_mean: f64,
    pub baseline_mean: f64,
    pub baseline_std: f64,
    /// current − baseline.
    pub delta: f64,
    pub ci: Option<(f64, f64)>,
    pub test: Option<TestResult>,
    pub worse: bool,
    pub drifted: bool,
    /// The previous run, against which regressions are counted.
    pub previous_run: Option<String>,
    pub regressions: Vec<Flip>,
    pub fixed: Vec<Flip>,
}

/// One bin or category of a distribution comparison, as shares of each sample.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Bin {
    pub label: String,
    pub baseline: f64,
    pub current: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Stable,
    Moderate,
    Drift,
}

/// The data-drift verdict for one feature of one run.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FeatureDrift {
    pub feature: String,
    pub kind: String,
    pub n_baseline: usize,
    pub n_current: usize,
    pub psi: Option<f64>,
    pub ks: Option<Ks>,
    pub js: Option<f64>,
    pub chi_squared: Option<ChiSquared>,
    pub level: Level,
    /// Why the usual measures are missing, when they are.
    pub note: Option<String>,
    pub bins: Vec<Bin>,
}

/// The result of a two-sample Kolmogorov–Smirnov test.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Ks {
    /// The largest gap between the two empirical distribution functions.
    pub d: f64,
    /// Asymptotic p-value.
    pub p: f64,
}

/// The result of a chi-squared test.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChiSquared {
    pub statistic: f64,
    pub df: f64,
    pub p: f64,
}
