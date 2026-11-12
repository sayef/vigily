//! Evaluations: datasets and their versions, per-item results and labels, feature samples,
//! schedule runs, alerts and suite baselines.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::drift::{BehaviourDrift, DriftConfig, FeatureDrift, Level, SuiteRun};
use crate::model::RunStatus;

/// An item as given by the user.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ItemInput {
    /// A stable id; derived from the input when absent.
    #[serde(default)]
    pub id: Option<String>,
    pub input: Value,
    #[serde(default)]
    pub expected: Value,
    #[serde(default)]
    pub metadata: Value,
}

/// A stored item.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub id: String,
    pub hash: String,
    pub input: Value,
    pub expected: Value,
    pub metadata: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ItemRef {
    pub id: String,
    pub hash: String,
}

/// How a new version relates to the previous one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CommitMode {
    /// The given items are the whole new version.
    #[default]
    Replace,
    /// The given items are added to, or replace items with the same id in, the latest version.
    Upsert,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Changes {
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
}

/// One immutable version of a dataset.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DatasetVersion {
    pub name: String,
    pub version: u32,
    pub created_at: i64,
    pub note: Option<String>,
    /// Hash of the item ids and hashes, in order.
    pub digest: String,
    pub items: Vec<ItemRef>,
    pub changes: Changes,
    /// Where the items came from: `sdk`, `jsonl:<file name>`, `csv:<file name>`.
    pub source: Option<String>,
    /// The storage (Lance) version the commit produced, when the backend is versioned.
    pub storage_version: Option<u64>,
    /// The storage tag that names `storage_version`, so version cleanup keeps it and audit
    /// records can cite exactly what was evaluated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_tag: Option<String>,
}

/// A dataset's summary.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DatasetInfo {
    pub name: String,
    pub description: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub latest: u32,
    pub items: usize,
}

/// One evaluator's verdict on one item.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Score {
    pub name: String,
    /// A number, usually 0 to 1; booleans are stored as 0 or 1.
    pub value: Option<f64>,
    pub passed: Option<bool>,
    pub reason: Option<String>,
    pub evaluator: String,
    pub evaluator_version: String,
}

/// The output and scores of one item in one eval run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EvalResult {
    pub run_id: String,
    pub item_id: String,
    pub item_hash: String,
    pub dataset: String,
    pub dataset_version: u32,
    pub output: Value,
    pub error: Option<String>,
    pub latency_ms: Option<f64>,
    pub scores: Vec<Score>,
    pub created_at: i64,
    /// The trace recorded while the target produced this output, when tracing was on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
}

/// A human judgement of one item's output, entered in the web app.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Label {
    pub run_id: String,
    pub item_id: String,
    /// The score it sets, such as `human` or `correct`.
    pub name: String,
    pub value: Option<f64>,
    pub passed: Option<bool>,
    pub comment: Option<String>,
    pub created_at: i64,
}

/// A sample of one input feature seen by a run, for data drift.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FeatureSample {
    pub run_id: String,
    pub feature: String,
    /// Numbers for a numeric feature, strings for a categorical one.
    pub values: Vec<Value>,
}

impl FeatureSample {
    /// Numbers, with null standing for a missing or non-finite number.
    pub fn is_numeric(&self) -> bool {
        self.values.iter().all(|v| v.is_number() || v.is_null()) && !self.values.is_empty()
    }

    pub fn numbers(&self) -> Vec<f64> {
        self.values.iter().filter_map(Value::as_f64).collect()
    }

    pub fn strings(&self) -> Vec<String> {
        self.values
            .iter()
            .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_string))
            .collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleRunStatus {
    Running,
    Succeeded,
    Failed,
    TimedOut,
    /// Not started: the previous run of the schedule was still going.
    SkippedOverlap,
    /// Not started: missed while the server was down, and the policy is to skip.
    SkippedMissed,
    /// Was running when the server stopped; its outcome is unknown.
    Interrupted,
}

/// One execution (attempt) of a scheduled command.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScheduleRun {
    /// `{started:020}-{attempt}`; sorts by time.
    pub id: String,
    pub schedule: String,
    /// The fire time this run belongs to (UTC microseconds).
    pub scheduled_for: i64,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub attempt: u32,
    pub status: ScheduleRunStatus,
    pub exit_code: Option<i32>,
    /// The command as configured, for the record.
    pub command: Vec<String>,
    /// The last 64 KiB of each stream.
    pub stdout: String,
    pub stderr: String,
    pub reason: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertKind {
    BehaviourDrift,
    DataDrift,
    ScheduleFailed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Warning,
    Critical,
}

/// Something an operator should look at.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Alert {
    pub id: String,
    pub kind: AlertKind,
    pub severity: Severity,
    pub title: String,
    pub detail: String,
    pub suite: Option<String>,
    pub run_id: Option<String>,
    pub schedule: Option<String>,
    pub created_at: i64,
    pub acknowledged_at: Option<i64>,
    /// Alerts with the same key are raised once.
    pub dedupe: String,
    /// Whether the webhook accepted it, when one is configured.
    pub delivered: Option<bool>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BaselineMode {
    /// The previous `window` runs of the suite.
    #[default]
    Rolling,
    /// A fixed set of runs.
    Pinned,
}

/// Which runs a suite's newest run is compared with.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Baseline {
    pub mode: BaselineMode,
    pub window: usize,
    pub runs: Vec<String>,
    pub updated_at: i64,
}

impl Default for Baseline {
    fn default() -> Self {
        Self {
            mode: BaselineMode::Rolling,
            window: 7,
            runs: vec![],
            updated_at: 0,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Answers of the evaluation routes

/// A dataset version without its item list.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VersionSummary {
    pub version: u32,
    pub created_at: i64,
    pub note: Option<String>,
    pub digest: String,
    pub changes: Changes,
    pub source: Option<String>,
    pub storage_version: Option<u64>,
    pub items: usize,
}

impl From<&DatasetVersion> for VersionSummary {
    fn from(v: &DatasetVersion) -> Self {
        Self {
            version: v.version,
            created_at: v.created_at,
            note: v.note.clone(),
            digest: v.digest.clone(),
            changes: v.changes.clone(),
            source: v.source.clone(),
            storage_version: v.storage_version,
            items: v.items.len(),
        }
    }
}

/// An eval run that used a dataset.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DatasetRun {
    pub id: String,
    pub name: String,
    pub created_at: i64,
    /// The dataset version the run used, as the SDK recorded it.
    pub version: Value,
    pub target: Value,
}

/// `GET datasets/{name}`: a dataset, one version's items (a page of them) and its eval runs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DatasetResponse {
    pub info: DatasetInfo,
    pub version: VersionSummary,
    pub versions: Vec<VersionSummary>,
    pub total: usize,
    pub items: Vec<Item>,
    /// Eval runs that used this dataset, newest first.
    pub runs: Vec<DatasetRun>,
}

/// One item of an eval run: the dataset item, the output, scores and labels.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ItemRow {
    pub item_id: String,
    pub input: Value,
    pub expected: Value,
    pub metadata: Value,
    pub output: Value,
    pub error: Option<String>,
    pub latency_ms: Option<f64>,
    pub scores: Vec<Score>,
    pub labels: Vec<Label>,
    pub failed: bool,
    /// The trace recorded for this item, when tracing was on.
    pub trace_id: Option<String>,
}

/// One score's mean and pass rate over a run's items.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ScoreSummary {
    pub mean: Option<f64>,
    pub pass_rate: Option<f64>,
    pub n: usize,
}

/// `GET runs/{id}/items`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ItemsResponse {
    pub run_id: String,
    pub dataset: Option<Value>,
    pub total: usize,
    pub failed: usize,
    pub errors: usize,
    pub scores: BTreeMap<String, ScoreSummary>,
    pub items: Vec<ItemRow>,
}

/// `POST runs/{id}/labels`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NewLabel {
    pub item_id: String,
    pub name: String,
    #[serde(default)]
    pub value: Option<f64>,
    #[serde(default)]
    pub passed: Option<bool>,
    #[serde(default)]
    pub comment: Option<String>,
}

/// One run's side of a compared item.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Side {
    pub output: Value,
    pub error: Option<String>,
    pub scores: Vec<Score>,
    pub failed: bool,
}

/// One item of a comparison.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CompareRow {
    pub item_id: String,
    pub input: Value,
    pub expected: Value,
    pub a: Option<Side>,
    pub b: Option<Side>,
    /// regressed, fixed, both_failed, both_passed, changed, only_a, only_b
    pub change: String,
}

/// A compared run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CompareRun {
    pub id: String,
    pub name: String,
    pub created_at: i64,
    pub target: Value,
    pub dataset: Option<Value>,
}

/// `GET compare?a=&b=`: two eval runs item by item.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CompareResponse {
    pub a: CompareRun,
    pub b: CompareRun,
    /// Items per kind of change.
    pub counts: BTreeMap<String, usize>,
    pub items: Vec<CompareRow>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MissedPolicy {
    /// Record that fires were missed and wait for the next one.
    #[default]
    Skip,
    /// Run once for the latest missed fire.
    RunOnce,
}

/// A schedule run without its output.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScheduleRunSummary {
    pub id: String,
    pub schedule: String,
    pub scheduled_for: i64,
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub attempt: u32,
    pub status: ScheduleRunStatus,
    pub exit_code: Option<i32>,
    pub reason: Option<String>,
    pub has_output: bool,
}

impl ScheduleRunSummary {
    pub fn of(r: &ScheduleRun, has_output: bool) -> Self {
        Self {
            id: r.id.clone(),
            schedule: r.schedule.clone(),
            scheduled_for: r.scheduled_for,
            started_at: r.started_at,
            ended_at: r.ended_at,
            attempt: r.attempt,
            status: r.status,
            exit_code: r.exit_code,
            reason: r.reason.clone(),
            has_output,
        }
    }
}

/// A schedule as configured, with its next fire time and recent runs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Schedule {
    pub name: String,
    pub description: Option<String>,
    pub cron: String,
    pub timezone: String,
    pub command: Vec<String>,
    /// The directory the command runs in.
    pub workdir: String,
    /// A duration such as `15m`.
    pub timeout: String,
    pub retries: u32,
    pub missed: MissedPolicy,
    pub enabled: bool,
    /// UTC microseconds; null while the schedule is not running.
    pub next_run: Option<i64>,
    pub last_run: Option<ScheduleRunSummary>,
    /// The newest runs, newest first (at most 14).
    pub recent: Vec<ScheduleRunSummary>,
    /// Runs kept.
    pub runs: usize,
}

/// `GET schedules`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SchedulesResponse {
    /// The configuration file the schedules come from.
    pub config_path: Option<String>,
    pub schedules: Vec<Schedule>,
}

/// An eval run a schedule run logged.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LinkedRun {
    pub id: String,
    pub name: String,
    pub status: RunStatus,
    pub target: Value,
}

/// A schedule run with the eval runs it logged.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScheduleRunListed {
    #[serde(flatten)]
    pub run: ScheduleRunSummary,
    pub eval_runs: Vec<LinkedRun>,
}

/// `GET schedules/{name}/runs`: newest first.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScheduleRunsResponse {
    pub runs: Vec<ScheduleRunListed>,
}

/// `GET schedules/{name}/runs/{id}`: one run with its output.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScheduleRunDetail {
    #[serde(flatten)]
    pub run: ScheduleRun,
    pub eval_runs: Vec<LinkedRun>,
}

/// A suite's latest behaviour verdict for one score.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Verdict {
    pub score: String,
    pub drifted: bool,
    pub delta: f64,
    pub p: Option<f64>,
    pub regressions: usize,
}

/// One suite in `GET suites`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SuiteSummary {
    pub key: String,
    pub label: String,
    pub experiment: Option<String>,
    pub target: Value,
    pub dataset: Option<String>,
    pub schedule: Option<String>,
    /// The number of runs.
    pub runs: usize,
    pub scores: Vec<String>,
    /// The newest finished run.
    pub latest: Option<SuiteRun>,
    pub baseline: Baseline,
    pub behaviour: Vec<Verdict>,
    /// The worst data-drift level of the newest finished run.
    pub data: Option<Level>,
}

/// One point of a score's history in `GET suites/{key}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HistoryPoint {
    pub run_id: String,
    pub created_at: i64,
    pub value: Option<f64>,
    pub baseline_mean: Option<f64>,
    pub baseline_low: Option<f64>,
    pub baseline_high: Option<f64>,
    pub drifted: bool,
    pub p: Option<f64>,
    pub regressions: usize,
}

/// The behaviour-drift verdict of the selected run, with the inputs of its regressed items.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DriftDetail {
    #[serde(flatten)]
    pub drift: BehaviourDrift,
    /// The input of each regressed item (the first 200), in the same order.
    pub regression_inputs: Vec<Value>,
}

/// `GET suites/{key}`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SuiteDetail {
    pub key: String,
    pub label: String,
    pub experiment: Option<String>,
    pub target: Value,
    pub dataset: Option<String>,
    pub schedule: Option<String>,
    pub scores: Vec<String>,
    pub runs: Vec<SuiteRun>,
    pub baseline: Baseline,
    pub config: DriftConfig,
    /// Score history by score name (the newest 120 finished runs).
    pub history: BTreeMap<String, Vec<HistoryPoint>>,
    pub selected: Option<String>,
    pub drift: Vec<DriftDetail>,
}

/// The data-drift thresholds.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Thresholds {
    pub psi_moderate: f64,
    pub psi_drift: f64,
    pub js_moderate: f64,
    pub js_drift: f64,
}

/// `GET suites/{key}/data-drift`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DataDriftResponse {
    pub run_id: String,
    pub baseline_runs: Vec<String>,
    pub features: Vec<FeatureDrift>,
    pub thresholds: Thresholds,
}

/// `POST alerts/{id}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AlertChange {
    pub acknowledged: bool,
}
