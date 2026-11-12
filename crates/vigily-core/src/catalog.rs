//! The catalog: an in-memory index of every run, its flattened parameters and a statistics summary
//! of every metric series, kept current by [`Catalog::refresh`].
//!
//! The runs table filters and sorts thousands of runs on parameters and metric statistics on every
//! keystroke, so those must not touch storage. A refresh reads each table once, in bulk (runs,
//! parameters, series, and the series statistics the store keeps), and skips every table whose
//! stored version did not change since the last refresh. No metric points are read.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use parking_lot::RwLock;
use serde_json::Value;

use crate::error::Result;
use crate::model::{flatten, Agg, Micros, Run, RunStatus, Series, Summary};
use crate::query::{resolve, Field, Query, RunView, Val};
use crate::store::{now_micros, Store};

pub use vigily_api::catalog::{ExperimentInfo, MetricInfo, ParamInfo, SeriesEntry};

/// One run with everything the runs table needs.
#[derive(Clone, Debug)]
pub struct RunEntry {
    pub run: Run,
    pub params: Value,
    pub flat: BTreeMap<String, Value>,
    pub series: Vec<SeriesEntry>,
}

impl RunEntry {
    /// `training` unless the run was created with another kind.
    pub fn kind(&self) -> &str {
        self.run.kind()
    }

    /// The target reference the run was created with, or null.
    pub fn target(&self) -> Value {
        self.run.target()
    }

    /// The last sign of life: the newest metadata write (heartbeats included) or point.
    pub fn last_seen(&self) -> Micros {
        self.series
            .iter()
            .filter_map(|s| s.summary.last_time)
            .fold(self.run.updated_at, i64::max)
    }

    /// A running run whose process stopped sending heartbeats and points.
    pub fn stale(&self, now: Micros) -> bool {
        self.run.status == RunStatus::Running && now - self.last_seen() > crate::store::STALE_AFTER
    }

    pub fn series_by_key(&self, key: &str) -> Option<&SeriesEntry> {
        self.series.iter().find(|s| s.key == key)
    }
}

impl RunView for RunEntry {
    fn run(&self) -> &Run {
        &self.run
    }

    fn params(&self) -> &BTreeMap<String, Value> {
        &self.flat
    }

    fn metrics(&self) -> Box<dyn Iterator<Item = (&str, &Value, &Summary)> + '_> {
        Box::new(
            self.series
                .iter()
                .map(|s| (s.series.name.as_str(), &s.series.context, &s.summary)),
        )
    }
}

/// An immutable view of the repository at one refresh.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    /// Newest first.
    pub runs: Vec<Arc<RunEntry>>,
    index: HashMap<String, usize>,
    pub refreshed_at: Micros,
    /// The store's table versions this snapshot was read at, when the store is versioned.
    versions: Option<[u64; 4]>,
}

impl Snapshot {
    pub fn get(&self, id: &str) -> Option<&Arc<RunEntry>> {
        self.index.get(id).map(|i| &self.runs[*i])
    }

    /// Experiments with run counts per status and the newest run's creation time.
    pub fn experiments(&self) -> Vec<ExperimentInfo> {
        let mut map: BTreeMap<String, ExperimentInfo> = BTreeMap::new();
        for e in &self.runs {
            let name = e.run.experiment.clone().unwrap_or_default();
            let x = map.entry(name.clone()).or_insert_with(|| ExperimentInfo {
                name,
                ..Default::default()
            });
            x.runs += 1;
            if e.run.archived {
                x.archived += 1;
            }
            match e.run.status {
                RunStatus::Running => x.running += 1,
                RunStatus::Finished => x.finished += 1,
                RunStatus::Failed => x.failed += 1,
                RunStatus::Aborted => x.aborted += 1,
            }
            x.first_created = Some(
                x.first_created
                    .map_or(e.run.created_at, |v: i64| v.min(e.run.created_at)),
            );
            x.last_created = Some(
                x.last_created
                    .map_or(e.run.created_at, |v: i64| v.max(e.run.created_at)),
            );
            for s in &e.series {
                if !x.metrics.contains(&s.key) {
                    x.metrics.push(s.key.clone());
                }
            }
        }
        let mut out: Vec<ExperimentInfo> = map.into_values().collect();
        for x in &mut out {
            x.metrics.sort();
        }
        out.sort_by_key(|x| std::cmp::Reverse(x.last_created));
        out
    }

    /// Every metric key with its name, context and the number of runs that log it.
    pub fn metric_keys(&self) -> Vec<MetricInfo> {
        let mut map: BTreeMap<String, MetricInfo> = BTreeMap::new();
        for e in &self.runs {
            for s in &e.series {
                map.entry(s.key.clone()).or_insert_with(|| MetricInfo {
                    key: s.key.clone(),
                    name: s.series.name.clone(),
                    context: s.series.context.clone(),
                    runs: 0,
                    eval_runs: 0,
                });
                let m = map.get_mut(&s.key).expect("inserted above");
                m.runs += 1;
                if e.kind() == "eval" {
                    m.eval_runs += 1;
                }
            }
        }
        map.into_values().collect()
    }

    /// Every parameter key with the kinds of value it takes and the number of runs that set it.
    pub fn param_keys(&self) -> Vec<ParamInfo> {
        let mut map: BTreeMap<String, ParamInfo> = BTreeMap::new();
        for e in &self.runs {
            for (k, v) in &e.flat {
                let p = map.entry(k.clone()).or_insert_with(|| ParamInfo {
                    key: k.clone(),
                    kind: kind_of(v).to_string(),
                    runs: 0,
                    distinct: 0,
                    values: Vec::new(),
                });
                p.runs += 1;
                if p.kind != kind_of(v) {
                    p.kind = "mixed".into();
                }
                if p.values.len() <= 64 && !p.values.contains(v) {
                    p.values.push(v.clone());
                }
            }
        }
        map.into_values()
            .map(|mut p| {
                p.distinct = p.values.len();
                if p.values.len() > 64 {
                    p.values.clear();
                }
                p
            })
            .collect()
    }

    /// Runs matching `query`, sorted by `sort` (a column id) when given, newest first otherwise.
    pub fn select(
        &self,
        query: &Query,
        sort: Option<&str>,
        descending: bool,
        include_archived: bool,
    ) -> Vec<Arc<RunEntry>> {
        let now = now_micros();
        let mut out: Vec<Arc<RunEntry>> = self
            .runs
            .iter()
            .filter(|e| include_archived || !e.run.archived || mentions_archived(query))
            .filter(|e| query.matches(e.as_ref(), now))
            .cloned()
            .collect();
        if let Some(col) = sort.and_then(Column::parse) {
            let mut keyed: Vec<(Val, Arc<RunEntry>)> =
                out.into_iter().map(|e| (col.value(&e, now), e)).collect();
            keyed.sort_by(|(a, ea), (b, eb)| {
                cmp_vals(a, b, descending).then(eb.run.created_at.cmp(&ea.run.created_at))
            });
            out = keyed.into_iter().map(|(_, e)| e).collect();
        }
        out
    }
}

fn mentions_archived(q: &Query) -> bool {
    q.groups.iter().flatten().any(|t| {
        matches!(
            &t.kind,
            crate::query::TermKind::Compare {
                field: Field::Archived,
                ..
            }
        )
    })
}

fn kind_of(v: &Value) -> &'static str {
    match v {
        Value::Number(_) => "number",
        Value::String(_) => "text",
        Value::Bool(_) => "bool",
        Value::Null => "null",
        _ => "list",
    }
}

/// Orders values of one column: missing values last in both directions, numbers before text.
fn cmp_vals(a: &Val, b: &Val, descending: bool) -> std::cmp::Ordering {
    use std::cmp::Ordering::*;
    let rank = |v: &Val| match v {
        Val::Null => 2,
        Val::Num(x) if !x.is_finite() => 2,
        Val::Str(_) => 1,
        _ => 0,
    };
    match rank(a).cmp(&rank(b)) {
        Equal => {}
        o => return o,
    }
    let o = match (a, b) {
        (Val::Num(x), Val::Num(y)) => x.total_cmp(y),
        (Val::Str(x), Val::Str(y)) => natural(x, y),
        (Val::Bool(x), Val::Bool(y)) => x.cmp(y),
        (Val::Num(_), _) => Less,
        (_, Val::Num(_)) => Greater,
        _ => Equal,
    };
    if descending {
        o.reverse()
    } else {
        o
    }
}

/// Case-insensitive order that compares digit runs as numbers: `run-2` before `run-10`.
fn natural(a: &str, b: &str) -> std::cmp::Ordering {
    let (mut x, mut y) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (x.peek().copied(), y.peek().copied()) {
            (None, None) => return a.cmp(b),
            (None, _) => return std::cmp::Ordering::Less,
            (_, None) => return std::cmp::Ordering::Greater,
            (Some(c), Some(d)) if c.is_ascii_digit() && d.is_ascii_digit() => {
                let mut n = String::new();
                while let Some(c) = x.peek().copied().filter(char::is_ascii_digit) {
                    n.push(c);
                    x.next();
                }
                let mut m = String::new();
                while let Some(d) = y.peek().copied().filter(char::is_ascii_digit) {
                    m.push(d);
                    y.next();
                }
                let o = n
                    .trim_start_matches('0')
                    .len()
                    .cmp(&m.trim_start_matches('0').len())
                    .then_with(|| n.trim_start_matches('0').cmp(m.trim_start_matches('0')));
                if o != std::cmp::Ordering::Equal {
                    return o;
                }
            }
            (Some(c), Some(d)) => {
                let o = c.to_lowercase().cmp(d.to_lowercase());
                if o != std::cmp::Ordering::Equal {
                    return o;
                }
                x.next();
                y.next();
            }
        }
    }
}

/// A runs-table column: `run:name`, `param:hparams.lr` or `metric:min:loss[val]`.
#[derive(Clone, Debug, PartialEq)]
pub enum Column {
    Run(String),
    Param(String),
    Metric(Agg, String),
}

impl Column {
    pub fn parse(id: &str) -> Option<Column> {
        let (kind, rest) = id.split_once(':')?;
        match kind {
            "run" => Some(Column::Run(rest.to_string())),
            "param" => Some(Column::Param(rest.to_string())),
            "metric" => {
                let (agg, key) = rest.split_once(':')?;
                Some(Column::Metric(Agg::parse(agg)?, key.to_string()))
            }
            _ => None,
        }
    }

    pub fn value(&self, e: &RunEntry, now: Micros) -> Val {
        let first = |f: Field| resolve(&f, e, now).into_iter().next().unwrap_or(Val::Null);
        match self {
            Column::Run(f) => match f.as_str() {
                "name" => first(Field::Name),
                "status" => first(Field::Status),
                "experiment" => first(Field::Experiment),
                "duration" => first(Field::Duration),
                "created" | "created_at" => Val::Num(e.run.created_at as f64),
                "updated" | "updated_at" => Val::Num(e.run.updated_at as f64),
                "tags" => Val::Str(e.run.tags.join(", ")),
                "kind" => Val::Str(e.kind().to_string()),
                "target" => match e.target() {
                    Value::Null => Val::Null,
                    Value::Object(m) => Val::Str(
                        m.values()
                            .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_string))
                            .collect::<Vec<_>>()
                            .join(", "),
                    ),
                    v => Val::Str(v.to_string()),
                },
                "id" => Val::Str(e.run.id.clone()),
                _ => Val::Null,
            },
            Column::Param(k) => e.flat.get(k).map_or(Val::Null, Val::from_json),
            Column::Metric(agg, key) => e
                .series_by_key(key)
                .and_then(|s| agg.of(&s.summary))
                .map_or(Val::Null, Val::Num),
        }
    }
}

/// The catalog over a store.
pub struct Catalog {
    store: Arc<dyn Store>,
    snap: RwLock<Arc<Snapshot>>,
    refreshing: tokio::sync::Mutex<()>,
}

impl Catalog {
    pub fn new(store: Arc<dyn Store>) -> Self {
        Self {
            store,
            snap: RwLock::new(Arc::new(Snapshot::default())),
            refreshing: tokio::sync::Mutex::new(()),
        }
    }

    pub fn store(&self) -> &Arc<dyn Store> {
        &self.store
    }

    /// The latest snapshot. Cheap; never waits for a refresh.
    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.snap.read().clone()
    }

    /// Refreshes when the snapshot is older than `max_age_micros`.
    pub async fn refresh_if_older(&self, max_age_micros: i64) -> Result<Arc<Snapshot>> {
        if now_micros() - self.snapshot().refreshed_at <= max_age_micros {
            return Ok(self.snapshot());
        }
        self.refresh().await
    }

    /// Reloads what changed since the last refresh. Concurrent callers share one refresh.
    pub async fn refresh(&self) -> Result<Arc<Snapshot>> {
        let started = now_micros();
        let _guard = self.refreshing.lock().await;
        let old = self.snapshot();
        if old.refreshed_at >= started {
            return Ok(old);
        }
        let refreshed_at = now_micros();
        let versions = self.store.versions().await?;
        // Table i (runs, params, series, metrics) is unchanged since the last refresh.
        let same =
            |i: usize| matches!((versions, old.versions), (Some(a), Some(b)) if a[i] == b[i]);
        if (0..4).all(same) {
            let snap = Arc::new(Snapshot {
                refreshed_at,
                ..Snapshot::clone(&old)
            });
            *self.snap.write() = snap.clone();
            return Ok(snap);
        }

        let runs: Vec<Run> = if same(0) {
            old.runs.iter().map(|e| e.run.clone()).collect()
        } else {
            self.store.runs().await?
        };
        let mut params = if same(1) {
            None
        } else {
            Some(self.store.all_params().await?)
        };
        let mut series = if same(2) {
            None
        } else {
            let mut by_run: HashMap<String, Vec<Series>> = HashMap::new();
            for s in self.store.all_series().await? {
                by_run.entry(s.run_id.clone()).or_default().push(s);
            }
            Some(by_run)
        };
        let stats = if same(3) {
            None
        } else {
            Some(self.store.series_stats().await?)
        };

        let mut entries: Vec<Arc<RunEntry>> = Vec::with_capacity(runs.len());
        for run in runs {
            let prev = old.get(&run.id);
            let (params, flat) = match params.as_mut() {
                Some(all) => {
                    let p = all
                        .remove(&run.id)
                        .unwrap_or_else(|| Value::Object(Default::default()));
                    let mut flat_v = Vec::new();
                    flatten("", &p, &mut flat_v);
                    let flat = flat_v.into_iter().filter(|(k, _)| !k.is_empty()).collect();
                    (p, flat)
                }
                None => prev
                    .map(|e| (e.params.clone(), e.flat.clone()))
                    .unwrap_or_else(|| (Value::Object(Default::default()), BTreeMap::new())),
            };
            let list: Vec<Series> = match series.as_mut() {
                Some(all) => all.remove(&run.id).unwrap_or_default(),
                None => prev
                    .map(|e| e.series.iter().map(|s| s.series.clone()).collect())
                    .unwrap_or_default(),
            };
            let mut series_entries: Vec<SeriesEntry> = list
                .into_iter()
                .map(|s| {
                    let summary = match &stats {
                        Some(st) => st.get(&s.id).cloned().unwrap_or_default(),
                        None => prev
                            .and_then(|p| p.series.iter().find(|x| x.series.id == s.id))
                            .map(|x| x.summary.clone())
                            .unwrap_or_default(),
                    };
                    SeriesEntry {
                        key: s.key(),
                        series: s,
                        summary,
                    }
                })
                .collect();
            series_entries.sort_by(|a, b| a.key.cmp(&b.key));
            entries.push(Arc::new(RunEntry {
                run,
                params,
                flat,
                series: series_entries,
            }));
        }

        let index = entries
            .iter()
            .enumerate()
            .map(|(i, e)| (e.run.id.clone(), i))
            .collect();
        let snap = Arc::new(Snapshot {
            runs: entries,
            index,
            refreshed_at,
            versions,
        });
        *self.snap.write() = snap.clone();
        Ok(snap)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::MemoryStore;
    use crate::model::{NewRun, RunPatch, TrackOpts};
    use serde_json::json;

    async fn seed(store: &MemoryStore) -> Vec<String> {
        let mut ids = Vec::new();
        for (i, lr) in [0.1, 0.01, 0.001].iter().enumerate() {
            let w = store
                .create_run(NewRun {
                    name: Some(format!("run-{}", [2, 10, 1][i])),
                    experiment: Some(if i < 2 { "mnist" } else { "cifar" }.into()),
                    created_at: Some(1_000 * i as i64),
                    ..Default::default()
                })
                .await
                .unwrap();
            w.set_params(&json!({"hparams": {"lr": lr}, "seed": i}))
                .unwrap();
            for s in 0..10 {
                w.track(
                    "loss",
                    lr * 10.0 / (s as f64 + 1.0),
                    TrackOpts {
                        context: Some(json!({"subset": "val"})),
                        step: Some(s),
                        ..Default::default()
                    },
                )
                .unwrap();
            }
            if i < 2 {
                w.finish(RunStatus::Finished, None).await.unwrap();
            }
            ids.push(w.info().id);
        }
        ids
    }

    #[tokio::test]
    async fn refresh_indexes_and_extends_running_runs() {
        let store = MemoryStore::new();
        let ids = seed(&store).await;
        let cat = Catalog::new(Arc::new(store.clone()));
        let snap = cat.refresh().await.unwrap();
        assert_eq!(snap.runs.len(), 3);
        let e = snap.get(&ids[2]).unwrap();
        assert_eq!(e.flat["hparams.lr"], json!(0.001));
        let s = e.series_by_key("loss[val]").unwrap();
        assert_eq!(s.summary.count, 10);

        // The running run gets more points; the next refresh extends its summary.
        let w = store.open_run(&ids[2]).await.unwrap();
        w.track(
            "loss",
            -1.0,
            TrackOpts {
                context: Some(json!({"subset": "val"})),
                ..Default::default()
            },
        )
        .unwrap();
        let snap = cat.refresh().await.unwrap();
        let s = snap
            .get(&ids[2])
            .unwrap()
            .series_by_key("loss[val]")
            .unwrap();
        assert_eq!((s.summary.count, s.summary.min), (11, Some(-1.0)));

        // A metadata change reloads the run.
        store
            .update_run(
                &ids[0],
                RunPatch {
                    tags: Some(vec!["best".into()]),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let snap = cat.refresh().await.unwrap();
        assert_eq!(snap.get(&ids[0]).unwrap().run.tags, vec!["best"]);
        assert_eq!(snap.experiments().len(), 2);
        assert_eq!(snap.metric_keys()[0].runs, 3);
        let p = snap.param_keys();
        assert_eq!(
            p.iter().find(|p| p.key == "hparams.lr").unwrap().distinct,
            3
        );
    }

    #[tokio::test]
    async fn select_filters_and_sorts() {
        let store = MemoryStore::new();
        let ids = seed(&store).await;
        let cat = Catalog::new(Arc::new(store));
        let snap = cat.refresh().await.unwrap();
        let q = Query::parse("exp:mnist").unwrap();
        let r = snap.select(&q, Some("param:hparams.lr"), false, false);
        assert_eq!(
            r.iter().map(|e| e.run.id.clone()).collect::<Vec<_>>(),
            vec![ids[1].clone(), ids[0].clone()]
        );
        let all = Query::default();
        let by_name: Vec<String> = snap
            .select(&all, Some("run:name"), false, false)
            .iter()
            .map(|e| e.run.display_name())
            .collect();
        assert_eq!(by_name, vec!["run-1", "run-2", "run-10"]);
        let by_loss = snap.select(&all, Some("metric:min:loss[val]"), true, false);
        assert_eq!(by_loss[0].run.id, ids[0]);
        assert_eq!(
            snap.select(
                &Query::parse("loss[val].last<=0.01").unwrap(),
                None,
                false,
                false
            )
            .len(),
            2
        );
        assert_eq!(
            Column::parse("metric:min:loss[val]"),
            Some(Column::Metric(Agg::Min, "loss[val]".into()))
        );
        assert_eq!(Column::parse("nope"), None);
    }
}
