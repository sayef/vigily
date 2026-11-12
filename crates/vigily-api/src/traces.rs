//! Traces: spans as sent and as stored, trace summaries, the payload policy and the price table.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// What is stored of a span's inputs, outputs, error messages and string attributes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PayloadMode {
    /// Everything, as sent.
    Full,
    /// Each field cut to `max_field_bytes` (4 KiB by default).
    #[default]
    Truncated,
    /// No inputs, outputs or string attributes; error messages keep only their type.
    Metadata,
}

impl PayloadMode {
    /// How much a mode keeps: the smaller, the stricter.
    fn openness(self) -> u8 {
        match self {
            Self::Metadata => 0,
            Self::Truncated => 1,
            Self::Full => 2,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Truncated => "truncated",
            Self::Metadata => "metadata",
        }
    }
}

/// A model's price, per million tokens, in the configured currency.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Price {
    /// Per million input (prompt) tokens.
    pub input: f64,
    /// Per million output (completion) tokens.
    pub output: f64,
    /// Per million cached input tokens; the input price when not set.
    #[serde(default)]
    pub cached_input: Option<f64>,
}

/// The payload policy of one project (an experiment name), overriding the global one.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectTracing {
    #[serde(default)]
    pub payloads: Option<PayloadMode>,
    #[serde(default)]
    pub max_field_bytes: Option<usize>,
}

/// The `[tracing]` section of `vigily.toml`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TracingConfig {
    /// What is stored of payloads: `full`, `truncated` (the default) or `metadata`.
    #[serde(default)]
    pub payloads: PayloadMode,
    /// The longest stored field in `truncated` mode, in bytes.
    #[serde(default = "default_max_field_bytes")]
    pub max_field_bytes: usize,
    /// The currency of the price table (shown with every cost).
    #[serde(default = "default_currency")]
    pub currency: String,
    /// Prices by model name. A name ending in `*` matches every model that starts with the rest;
    /// an exact name wins over a pattern, and a longer pattern over a shorter one.
    #[serde(default)]
    pub prices: BTreeMap<String, Price>,
    /// Payload policies by project (the trace's `project`, or its run's experiment).
    #[serde(default)]
    pub projects: BTreeMap<String, ProjectTracing>,
}

fn default_max_field_bytes() -> usize {
    4096
}
fn default_currency() -> String {
    "USD".into()
}

impl Default for TracingConfig {
    fn default() -> Self {
        Self {
            payloads: PayloadMode::default(),
            max_field_bytes: default_max_field_bytes(),
            currency: default_currency(),
            prices: BTreeMap::new(),
            projects: BTreeMap::new(),
        }
    }
}

impl TracingConfig {
    /// The payload mode and field limit for `project`.
    pub fn policy(&self, project: Option<&str>) -> (PayloadMode, usize) {
        let p = project.and_then(|p| self.projects.get(p));
        (
            p.and_then(|p| p.payloads).unwrap_or(self.payloads),
            p.and_then(|p| p.max_field_bytes)
                .unwrap_or(self.max_field_bytes),
        )
    }

    /// The policy for a trace whose project is not known yet: the strictest of the global policy
    /// and every project's, so nothing is stored that the project's policy would not allow.
    pub fn strictest(&self) -> (PayloadMode, usize) {
        let mut out = (self.payloads, self.max_field_bytes);
        for name in self.projects.keys() {
            let (m, n) = self.policy(Some(name));
            if m.openness() < out.0.openness() {
                out.0 = m;
            }
            out.1 = out.1.min(n);
        }
        out
    }

    /// The price of `model`, exact names first, then the longest matching `prefix*` pattern.
    pub fn price(&self, model: &str) -> Option<&Price> {
        if let Some(p) = self.prices.get(model) {
            return Some(p);
        }
        self.prices
            .iter()
            .filter_map(|(k, p)| {
                let stem = k.strip_suffix('*')?;
                model.starts_with(stem).then_some((stem.len(), p))
            })
            .max_by_key(|(n, _)| *n)
            .map(|(_, p)| p)
    }

    pub fn validate(&self) -> std::result::Result<(), String> {
        if self.max_field_bytes < 16 {
            return Err("tracing.max_field_bytes must be at least 16".into());
        }
        if self.currency.trim().is_empty() || self.currency.len() > 8 {
            return Err("tracing.currency must be a short code such as EUR or USD".into());
        }
        for (m, p) in &self.prices {
            let ok = |x: f64| x.is_finite() && x >= 0.0;
            if !ok(p.input) || !ok(p.output) || !p.cached_input.is_none_or(ok) {
                return Err(format!(
                    "tracing.prices.\"{m}\": prices must be finite and not negative"
                ));
            }
        }
        for (name, p) in &self.projects {
            if p.max_field_bytes.is_some_and(|n| n < 16) {
                return Err(format!(
                    "tracing.projects.\"{name}\".max_field_bytes must be at least 16"
                ));
            }
        }
        Ok(())
    }
}

/// Token usage of an LLM call.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens: Option<u64>,
    /// Input tokens served from the provider's prompt cache (part of `input_tokens`).
    #[serde(default)]
    pub cached_tokens: Option<u64>,
}

impl Usage {
    pub fn is_empty(&self) -> bool {
        self.input_tokens.is_none() && self.output_tokens.is_none() && self.cached_tokens.is_none()
    }
}

/// A span as it is sent to the server.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SpanIn {
    pub trace_id: String,
    pub span_id: String,
    #[serde(default)]
    pub parent_id: Option<String>,
    pub name: String,
    /// `llm`, `tool`, `retrieval`, `chain`, `embedding`, `agent` or another short word.
    #[serde(default)]
    pub kind: Option<String>,
    /// UTC microseconds.
    pub start_time: i64,
    #[serde(default)]
    pub end_time: Option<i64>,
    /// `ok`, `error` or `unset`.
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub attributes: Map<String, Value>,
    /// The run the trace belongs to.
    #[serde(default)]
    pub run_id: Option<String>,
    /// The evaluation item the trace was recorded for.
    #[serde(default)]
    pub eval_item: Option<String>,
    /// The project whose payload policy applies; the run's experiment when not set.
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub input: Option<Value>,
    #[serde(default)]
    pub output: Option<Value>,
    #[serde(default)]
    pub usage: Option<Usage>,
}

/// A stored span.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Span {
    pub span_id: String,
    #[serde(default)]
    pub parent_id: Option<String>,
    pub name: String,
    pub kind: String,
    pub start_time: i64,
    pub end_time: i64,
    pub status: String,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub attributes: Map<String, Value>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub input: Option<Value>,
    #[serde(default)]
    pub output: Option<Value>,
    #[serde(default)]
    pub usage: Option<Usage>,
    /// In `currency`; null when the model has no price.
    #[serde(default)]
    pub cost: Option<f64>,
    /// The currency of `cost` (the price table's when the span was stored).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    /// The payload mode in force when the span was stored.
    pub payloads: PayloadMode,
    /// The trace's project was not known when the span arrived, so the strictest configured
    /// policy applied instead of the project's.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub provisional: bool,
    /// Fields cut in `truncated` mode, with their original size in bytes (a field left out
    /// because the span's payload budget was spent is listed too).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub truncated: BTreeMap<String, usize>,
    /// Attributes left out (beyond the count limit, keys too long, or not identifier-like in
    /// `metadata` mode).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub dropped_attributes: u32,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// A trace's summary row.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TraceSummary {
    pub id: String,
    /// The root span's name (the first span's until the root arrives).
    pub name: String,
    #[serde(default)]
    pub root: Option<String>,
    #[serde(default)]
    pub run_id: Option<String>,
    #[serde(default)]
    pub eval_item: Option<String>,
    #[serde(default)]
    pub project: Option<String>,
    pub start_time: i64,
    pub end_time: i64,
    /// `error` when any span failed, else `ok`.
    pub status: String,
    pub spans: u32,
    pub errors: u32,
    pub llm_calls: u32,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    /// The sum of the priced spans' costs; null when no span had a price.
    #[serde(default)]
    pub cost: Option<f64>,
    /// LLM spans with token usage but no price for their model, or priced in another currency
    /// than the trace's (see `mixed_currency`).
    pub unpriced: u32,
    pub currency: String,
    /// Some spans were priced after the configured currency changed; their cost is not in `cost`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub mixed_currency: bool,
    pub models: Vec<String>,
    /// The payload mode of the latest batch.
    pub payloads: PayloadMode,
    /// The number of span chunks (`tspan/<id>/<n>`).
    pub chunks: u32,
}

impl TraceSummary {
    pub fn duration_us(&self) -> i64 {
        self.end_time.saturating_sub(self.start_time).max(0)
    }
}

/// What an ingest stored.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct IngestReport {
    pub accepted: usize,
    /// Spans ignored because their id was stored before.
    pub duplicates: usize,
    pub traces: usize,
    /// Traces stored for the first time.
    pub new_traces: usize,
}

/// `POST traces/ingest`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct IngestBody {
    pub spans: Vec<SpanIn>,
}

/// A trace as a row of the traces list.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TraceRow {
    #[serde(flatten)]
    pub summary: TraceSummary,
    pub run_name: Option<String>,
    pub duration_us: i64,
}

/// Totals over the traces matching a filter.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Totals {
    pub traces: usize,
    pub errors: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_tokens: u64,
    /// Cost per currency (a changed currency setting leaves older traces in the old one).
    pub cost: BTreeMap<String, f64>,
    pub unpriced: u32,
    /// Median and 95th-percentile duration in microseconds.
    pub p50_us: Option<i64>,
    pub p95_us: Option<i64>,
}

/// A run that has traces, for the run filter.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunRef {
    pub id: String,
    pub name: String,
}

/// `GET traces`: trace summaries, newest first, with totals and filter choices.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TracesResponse {
    pub total: usize,
    pub rows: Vec<TraceRow>,
    pub totals: Totals,
    /// Filter choices over all traces.
    pub models: Vec<String>,
    pub runs: Vec<RunRef>,
    /// The payload policy for new traces (global; projects may override it).
    pub payloads: PayloadMode,
    pub max_field_bytes: usize,
    pub currency: String,
}

/// `GET traces/{id}`: one trace with its spans.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TraceResponse {
    pub trace: TraceRow,
    pub spans: Vec<Span>,
    /// The field limit of the trace's payload policy now (for "truncated to N KiB").
    pub max_field_bytes: usize,
}

/// `DELETE traces/{id}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TraceDeleted {
    pub deleted: bool,
}

/// `POST traces/delete`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DeleteTraces {
    pub ids: Vec<String>,
    /// Purge storage afterwards, so the deleted spans are erased from the files too.
    #[serde(default)]
    pub erase: bool,
    /// With `erase`: also delete storage tags that keep erased data.
    #[serde(default)]
    pub force: bool,
}

/// What `POST traces/delete` did.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TracesDeleted {
    pub deleted: usize,
    /// Ids with no trace.
    pub missing: Vec<String>,
    /// The purge, with `erase`.
    pub purge: Option<crate::runs::PurgeSummary>,
}

/// A payload policy: what is stored of span payloads, and the longest stored field.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Policy {
    pub payloads: PayloadMode,
    pub max_field_bytes: usize,
}

/// `GET tracing/policy?project=`: the payload policy a client applies before sending spans, so
/// payloads the engine would drop never leave the process.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TracingPolicy {
    /// The project asked for; null for the global policy.
    pub project: Option<String>,
    /// The policy for that project (the global one without a project).
    #[serde(flatten)]
    pub policy: Policy,
    /// The policy while a trace's project is not known yet: the strictest configured.
    pub unknown_project: Policy,
    /// The currency of costs.
    pub currency: String,
}
