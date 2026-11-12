//! The domain model: runs, parameters, metric series and their points.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Microseconds since the Unix epoch (UTC).
pub type Micros = i64;

/// Lifecycle state of a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunStatus {
    Running,
    Finished,
    Failed,
    Aborted,
}

impl RunStatus {
    pub const ALL: [RunStatus; 4] = [
        RunStatus::Running,
        RunStatus::Finished,
        RunStatus::Failed,
        RunStatus::Aborted,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            RunStatus::Running => "running",
            RunStatus::Finished => "finished",
            RunStatus::Failed => "failed",
            RunStatus::Aborted => "aborted",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|v| v.as_str().eq_ignore_ascii_case(s.trim()))
    }
}

/// A run's metadata.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Run {
    pub id: String,
    pub name: Option<String>,
    pub experiment: Option<String>,
    pub description: Option<String>,
    pub tags: Vec<String>,
    pub status: RunStatus,
    pub archived: bool,
    pub created_at: Micros,
    pub updated_at: Micros,
    pub ended_at: Option<Micros>,
    /// What the run is: `training` when `None` (see [`Run::kind`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// What the run evaluates or trains, as a JSON object; `None` when not given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<Value>,
}

impl Run {
    /// `training` unless the run was created with another kind.
    pub fn kind(&self) -> &str {
        self.kind.as_deref().unwrap_or(DEFAULT_KIND)
    }

    /// The target reference the run was created with, or null.
    pub fn target(&self) -> Value {
        self.target.clone().unwrap_or(Value::Null)
    }

    /// The name, or the short id when the run has none.
    pub fn display_name(&self) -> String {
        match &self.name {
            Some(n) if !n.trim().is_empty() => n.clone(),
            _ => self.short_id(),
        }
    }

    /// The last eight characters of the id. Ids are time-ordered UUIDs whose leading characters
    /// are a timestamp shared by runs started together; the trailing ones are random.
    pub fn short_id(&self) -> String {
        let n = self.id.chars().count();
        self.id.chars().skip(n.saturating_sub(8)).collect()
    }

    /// Seconds from creation to the end, or to `now` while the run has no end.
    pub fn duration_secs(&self, now: Micros) -> f64 {
        let end = self.ended_at.unwrap_or(now);
        (end - self.created_at).max(0) as f64 / 1e6
    }
}

/// Fields for a new run.
#[derive(Clone, Debug, Default)]
pub struct NewRun {
    /// A run id; generated when `None`.
    pub id: Option<String>,
    pub name: Option<String>,
    pub experiment: Option<String>,
    pub description: Option<String>,
    pub tags: Vec<String>,
    /// The creation time, when importing a run that started earlier; now when `None`.
    pub created_at: Option<Micros>,
    /// What the run is: `training` (the default), `eval`, or another word.
    pub kind: Option<String>,
    /// What the run evaluates or trains: a free-form JSON object such as
    /// `{"model": "...", "prompt": "...", "endpoint": "...", "git": "<sha>"}`.
    pub target: Option<Value>,
}

/// The parameter key under which a run's kind and target are stored. They are run metadata, but
/// kv0's run table has no columns for them yet (docs/kv0-requests.md), so every store
/// keeps them as reserved parameters and the catalog reads them back from there.
/// Reserved parameters the SDK records about a run's origin (for example the schedule run that
/// started it). A run's kind and target are columns of the run itself.
pub const RUN_META: &str = "__run__";

/// The kind of a run without one.
pub const DEFAULT_KIND: &str = "training";

/// A run kind as stored: trimmed and lower case; `None` when empty.
pub fn normalize_kind(kind: Option<&str>) -> Option<String> {
    kind.map(str::trim)
        .filter(|k| !k.is_empty())
        .map(str::to_lowercase)
}

/// A target as stored: an object as given, a string as `{"ref": "..."}`; `None` for null.
pub fn normalize_target(target: Option<&Value>) -> Option<Value> {
    match target {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(serde_json::json!({ "ref": s })),
        Some(t) => Some(t.clone()),
    }
}

/// A change to a run's metadata. `None` leaves a field as it is.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub experiment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<RunStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<Micros>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<Micros>,
}

impl RunPatch {
    pub fn apply(&self, run: &mut Run) {
        if let Some(v) = &self.name {
            run.name = non_empty(v);
        }
        if let Some(v) = &self.experiment {
            run.experiment = non_empty(v);
        }
        if let Some(v) = &self.description {
            run.description = non_empty(v);
        }
        if let Some(v) = &self.tags {
            let mut tags: Vec<String> = Vec::new();
            for t in v.iter().map(|t| t.trim()).filter(|t| !t.is_empty()) {
                if !tags.iter().any(|x| x == t) {
                    tags.push(t.to_string());
                }
            }
            run.tags = tags;
        }
        if let Some(v) = self.archived {
            run.archived = v;
        }
        if let Some(v) = self.status {
            run.status = v;
        }
        if let Some(v) = self.created_at {
            run.created_at = v;
        }
        if let Some(v) = self.ended_at {
            run.ended_at = Some(v);
        }
    }
}

fn non_empty(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}

/// Optional fields of one tracked point.
#[derive(Clone, Debug, Default)]
pub struct TrackOpts {
    /// The step; one past the series' last step when `None`.
    pub step: Option<i64>,
    pub epoch: Option<i64>,
    /// A JSON object such as `{"subset": "train"}`; `None` is the empty context.
    pub context: Option<Value>,
    /// When the point was measured; now when `None`.
    pub timestamp: Option<Micros>,
}

/// One metric series: a metric name under one context in one run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Series {
    /// The storage id. Serialised as a string because it does not fit a JavaScript number.
    #[serde(with = "u64_string")]
    pub id: u64,
    pub run_id: String,
    pub name: String,
    pub context: Value,
}

impl Series {
    pub fn key(&self) -> String {
        metric_key(&self.name, &self.context)
    }
}

/// The key that names a metric across runs: `loss` for the empty context, `loss[val]` for
/// `{"subset": "val"}`, `loss[val, 0]` for two context values (in key order).
pub fn metric_key(name: &str, context: &Value) -> String {
    let values = context_values(context);
    if values.is_empty() {
        name.to_string()
    } else {
        format!("{name}[{}]", values.join(", "))
    }
}

/// The context's values in key order, as text.
pub fn context_values(context: &Value) -> Vec<String> {
    let Some(map) = context.as_object() else {
        return Vec::new();
    };
    let mut keys: Vec<&String> = map.keys().collect();
    keys.sort();
    keys.into_iter()
        .map(|k| match &map[k] {
            Value::String(s) => s.clone(),
            v => v.to_string(),
        })
        .collect()
}

/// Splits a metric key into its name and context part: `loss[val]` → (`loss`, Some(`val`)).
pub fn split_metric_key(key: &str) -> (&str, Option<&str>) {
    match key.strip_suffix(']').and_then(|k| k.split_once('[')) {
        Some((name, ctx)) => (name, Some(ctx)),
        None => (key, None),
    }
}

/// Whether `context` matches a context filter written as in a metric key: `val`, `val, 0` or
/// `subset=val`. Comparison ignores case and surrounding spaces.
pub fn context_matches(context: &Value, filter: &str) -> bool {
    let parts: Vec<&str> = filter
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    let values = context_values(context);
    if parts.is_empty() {
        return values.is_empty();
    }
    let map = context.as_object();
    parts.iter().all(|p| match p.split_once('=') {
        Some((k, v)) => map.and_then(|m| m.get(k.trim())).is_some_and(|x| {
            let s = match x {
                Value::String(s) => s.clone(),
                o => o.to_string(),
            };
            s.eq_ignore_ascii_case(v.trim())
        }),
        None => values.iter().any(|x| x.eq_ignore_ascii_case(p)),
    })
}

/// A step range: `start <= step < stop`; open ends when `None`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StepRange {
    pub start: Option<i64>,
    pub stop: Option<i64>,
}

/// A time range: `start <= timestamp < stop` in microseconds (UTC); open ends when `None`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TimeRange {
    pub start: Option<Micros>,
    pub stop: Option<Micros>,
}

impl TimeRange {
    pub fn is_open(&self) -> bool {
        self.start.is_none() && self.stop.is_none()
    }

    pub fn contains(&self, t: Micros) -> bool {
        self.start.is_none_or(|s| t >= s) && self.stop.is_none_or(|e| t < e)
    }
}

/// Points of one series, ordered by step.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Points {
    pub steps: Vec<i64>,
    pub values: Vec<f64>,
    pub epochs: Vec<Option<i64>>,
    pub timestamps: Vec<Micros>,
}

impl Points {
    pub fn len(&self) -> usize {
        self.steps.len()
    }

    /// Keeps the points whose timestamps fall in `time`.
    pub fn retain_time(&mut self, time: TimeRange) {
        if time.is_open() {
            return;
        }
        let keep: Vec<usize> = (0..self.len())
            .filter(|&i| time.contains(self.timestamps[i]))
            .collect();
        self.steps = keep.iter().map(|&i| self.steps[i]).collect();
        self.values = keep.iter().map(|&i| self.values[i]).collect();
        self.epochs = keep.iter().map(|&i| self.epochs[i]).collect();
        self.timestamps = keep.iter().map(|&i| self.timestamps[i]).collect();
    }

    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }
}

/// Running statistics of a series, extended as points arrive. Non-finite values count in `count`
/// and `non_finite` and are skipped by `min` and `max`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    pub count: u64,
    pub non_finite: u64,
    pub first_step: Option<i64>,
    pub last_step: Option<i64>,
    pub last: Option<f64>,
    pub min: Option<f64>,
    pub min_step: Option<i64>,
    pub max: Option<f64>,
    pub max_step: Option<i64>,
    pub last_epoch: Option<i64>,
    pub first_time: Option<Micros>,
    pub last_time: Option<Micros>,
}

impl Summary {
    /// Adds points that come after every point seen so far.
    pub fn extend(&mut self, p: &Points) {
        for i in 0..p.len() {
            let (step, v) = (p.steps[i], p.values[i]);
            if self.last_step.is_some_and(|s| step <= s) {
                continue;
            }
            self.count += 1;
            self.first_step.get_or_insert(step);
            self.first_time.get_or_insert(p.timestamps[i]);
            self.last_step = Some(step);
            self.last_time = Some(p.timestamps[i]);
            self.last_epoch = p.epochs[i].or(self.last_epoch);
            if !v.is_finite() {
                self.non_finite += 1;
                self.last = None;
                continue;
            }
            self.last = Some(v);
            if self.min.is_none_or(|m| v < m) {
                self.min = Some(v);
                self.min_step = Some(step);
            }
            if self.max.is_none_or(|m| v > m) {
                self.max = Some(v);
                self.max_step = Some(step);
            }
        }
    }

    pub fn of(p: &Points) -> Self {
        let mut s = Self::default();
        s.extend(p);
        s
    }
}

/// Which statistic of a series a column or filter refers to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Agg {
    Last,
    Min,
    Max,
    Count,
}

impl Agg {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "last" => Agg::Last,
            "min" => Agg::Min,
            "max" => Agg::Max,
            "count" => Agg::Count,
            _ => return None,
        })
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Agg::Last => "last",
            Agg::Min => "min",
            Agg::Max => "max",
            Agg::Count => "count",
        }
    }

    pub fn of(&self, s: &Summary) -> Option<f64> {
        match self {
            Agg::Last => s.last,
            Agg::Min => s.min,
            Agg::Max => s.max,
            Agg::Count => Some(s.count as f64),
        }
    }
}

/// Flattens a JSON object into dotted keys; arrays and scalars are leaves.
pub fn flatten(prefix: &str, v: &Value, out: &mut Vec<(String, Value)>) {
    match v {
        Value::Object(m) if !m.is_empty() || prefix.is_empty() => {
            for (k, x) in m {
                let key = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                flatten(&key, x, out);
            }
        }
        _ => out.push((prefix.to_string(), v.clone())),
    }
}

/// Merges `patch` into `target` key by key (objects merge, everything else replaces).
pub fn merge_json(target: &mut Value, patch: &Value) {
    match (target, patch) {
        (Value::Object(t), Value::Object(p)) => {
            for (k, v) in p {
                match t.get_mut(k) {
                    Some(x) if x.is_object() && v.is_object() => merge_json(x, v),
                    _ => {
                        t.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (t, p) => *t = p.clone(),
    }
}

/// Builds a nested object from a dotted key and a value.
pub fn nest(key: &str, value: Value) -> Value {
    key.rsplit('.').fold(value, |acc, part| {
        let mut m = serde_json::Map::new();
        m.insert(part.to_string(), acc);
        Value::Object(m)
    })
}

pub(crate) mod u64_string {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &u64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&v.to_string())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn metric_keys_sort_context_values() {
        assert_eq!(metric_key("loss", &json!({})), "loss");
        assert_eq!(metric_key("loss", &json!({"subset": "val"})), "loss[val]");
        assert_eq!(
            metric_key("acc", &json!({"subset": "val", "head": 2})),
            "acc[2, val]"
        );
        assert_eq!(split_metric_key("acc[2, val]"), ("acc", Some("2, val")));
        assert_eq!(split_metric_key("loss"), ("loss", None));
    }

    #[test]
    fn context_filters() {
        let c = json!({"subset": "val", "head": 2});
        assert!(context_matches(&c, "val"));
        assert!(context_matches(&c, "subset=VAL"));
        assert!(context_matches(&c, "val, 2"));
        assert!(!context_matches(&c, "train"));
        assert!(context_matches(&json!({}), ""));
    }

    #[test]
    fn summary_skips_non_finite_and_old_steps() {
        let p = Points {
            steps: vec![0, 1, 2],
            values: vec![3.0, f64::NAN, 1.0],
            epochs: vec![None, Some(0), None],
            timestamps: vec![10, 20, 30],
        };
        let mut s = Summary::of(&p);
        assert_eq!(s.count, 3);
        assert_eq!(s.non_finite, 1);
        assert_eq!((s.min, s.min_step, s.max), (Some(1.0), Some(2), Some(3.0)));
        assert_eq!(s.last_epoch, Some(0));
        s.extend(&p);
        assert_eq!(s.count, 3);
    }

    #[test]
    fn patch_dedupes_tags_and_clears_empty_names() {
        let mut r = Run {
            id: "abc".into(),
            name: Some("x".into()),
            experiment: None,
            description: None,
            tags: vec![],
            status: RunStatus::Running,
            archived: false,
            created_at: 0,
            updated_at: 0,
            ended_at: None,
            kind: None,
            target: None,
        };
        RunPatch {
            name: Some(" ".into()),
            tags: Some(vec!["a".into(), " a ".into(), "b".into()]),
            ..Default::default()
        }
        .apply(&mut r);
        assert_eq!(r.name, None);
        assert_eq!(r.tags, vec!["a", "b"]);
        assert_eq!(r.display_name(), "abc");
        r.id = "0123456789abcdef".into();
        assert_eq!(r.short_id(), "89abcdef");
    }

    #[test]
    fn flatten_and_nest() {
        let mut out = Vec::new();
        flatten("", &json!({"a": {"b": 1, "c": [1, 2]}, "d": {}}), &mut out);
        assert_eq!(
            out,
            vec![
                ("a.b".to_string(), json!(1)),
                ("a.c".to_string(), json!([1, 2])),
                ("d".to_string(), json!({}))
            ]
        );
        let mut t = json!({"a": {"b": 1}});
        merge_json(&mut t, &nest("a.c", json!(2)));
        assert_eq!(t, json!({"a": {"b": 1, "c": 2}}));
    }
}
