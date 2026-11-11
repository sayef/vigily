//! An in-memory [`Store`], for tests and for embedding vigily without storage.

use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::Value;

use crate::error::{Error, Result};
use crate::model::{
    flatten, merge_json, nest, NewRun, Points, Run, RunPatch, RunStatus, Series, StepRange,
    TrackOpts,
};
use crate::store::{now_micros, RunWriter, Store};

/// A stored point: value, epoch and timestamp, keyed by step.
type Point = (f64, Option<i64>, i64);

#[derive(Default)]
struct State {
    runs: BTreeMap<String, Run>,
    params: HashMap<String, Value>,
    series: BTreeMap<u64, Series>,
    points: HashMap<u64, BTreeMap<i64, Point>>,
    next_id: u64,
}

/// A repository held in memory. Cloning shares the data.
#[derive(Clone, Default)]
pub struct MemoryStore {
    state: Arc<Mutex<State>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets when a run was last written, as if its process had gone quiet (for tests of stale runs).
    pub fn set_updated_at(&self, run_id: &str, at: crate::model::Micros) {
        if let Some(r) = self.state.lock().runs.get_mut(run_id) {
            r.updated_at = at;
        }
    }

    fn create_bare(&self, new: NewRun) -> Result<Box<dyn RunWriter>> {
        let now = now_micros();
        let id = {
            let mut s = self.state.lock();
            s.next_id += 1;
            let id = new
                .id
                .unwrap_or_else(|| format!("{:016x}{:016x}", now, s.next_id));
            if s.runs.contains_key(&id) {
                return Err(Error::InvalidArgument(format!("run {id} already exists")));
            }
            s.runs.insert(
                id.clone(),
                Run {
                    id: id.clone(),
                    name: new.name,
                    experiment: new.experiment,
                    description: new.description,
                    tags: new.tags,
                    status: RunStatus::Running,
                    archived: false,
                    created_at: new.created_at.unwrap_or(now),
                    updated_at: now,
                    ended_at: None,
                    kind: crate::model::normalize_kind(new.kind.as_deref()),
                    target: crate::model::normalize_target(new.target.as_ref()),
                },
            );
            id
        };
        Ok(self.writer(id))
    }

    fn writer(&self, run_id: String) -> Box<dyn RunWriter> {
        Box::new(MemoryRun {
            state: self.state.clone(),
            run_id,
        })
    }
}

/// The id of a series, from its run, name and context.
pub fn series_hash(run_id: &str, name: &str, context: &Value) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    run_id.hash(&mut h);
    name.hash(&mut h);
    let mut flat = Vec::new();
    flatten("", context, &mut flat);
    flat.sort_by(|a, b| a.0.cmp(&b.0));
    for (k, v) in flat {
        k.hash(&mut h);
        v.to_string().hash(&mut h);
    }
    h.finish()
}

#[async_trait]
impl Store for MemoryStore {
    fn location(&self) -> String {
        "memory".into()
    }

    async fn runs(&self) -> Result<Vec<Run>> {
        let s = self.state.lock();
        let mut runs: Vec<Run> = s.runs.values().cloned().collect();
        runs.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(b.id.cmp(&a.id)));
        Ok(runs)
    }

    async fn params(&self, run_id: &str) -> Result<Value> {
        Ok(self
            .state
            .lock()
            .params
            .get(run_id)
            .cloned()
            .unwrap_or_else(|| Value::Object(Default::default())))
    }

    async fn series(&self, run_id: &str) -> Result<Vec<Series>> {
        Ok(self
            .state
            .lock()
            .series
            .values()
            .filter(|s| s.run_id == run_id)
            .cloned()
            .collect())
    }

    async fn points(&self, ids: &[u64], range: StepRange) -> Result<HashMap<u64, Points>> {
        let s = self.state.lock();
        let mut out = HashMap::new();
        for id in ids {
            let Some(pts) = s.points.get(id) else {
                continue;
            };
            let mut p = Points::default();
            let lo = range.start.unwrap_or(i64::MIN);
            let hi = range.stop.unwrap_or(i64::MAX);
            for (step, (v, e, t)) in pts.range(lo..hi) {
                p.steps.push(*step);
                p.values.push(*v);
                p.epochs.push(*e);
                p.timestamps.push(*t);
            }
            if !p.is_empty() {
                out.insert(*id, p);
            }
        }
        Ok(out)
    }

    async fn create_run(&self, new: NewRun) -> Result<Box<dyn RunWriter>> {
        self.create_bare(new)
    }

    async fn open_run(&self, run_id: &str) -> Result<Box<dyn RunWriter>> {
        if !self.state.lock().runs.contains_key(run_id) {
            return Err(Error::NotFound(format!("run {run_id}")));
        }
        Ok(self.writer(run_id.to_string()))
    }

    async fn delete_runs(&self, run_ids: &[String]) -> Result<usize> {
        let mut s = self.state.lock();
        let mut n = 0;
        for id in run_ids {
            if s.runs.remove(id).is_some() {
                n += 1;
            }
            s.params.remove(id);
            let gone: Vec<u64> = s
                .series
                .values()
                .filter(|x| &x.run_id == id)
                .map(|x| x.id)
                .collect();
            for sid in gone {
                s.series.remove(&sid);
                s.points.remove(&sid);
            }
        }
        Ok(n)
    }

    async fn update_run(&self, run_id: &str, patch: RunPatch) -> Result<Run> {
        let w = self.open_run(run_id).await?;
        w.update(patch).await?;
        Ok(w.info())
    }

    async fn flush(&self) -> Result<()> {
        Ok(())
    }

    async fn close(&self) -> Result<()> {
        Ok(())
    }
}

struct MemoryRun {
    state: Arc<Mutex<State>>,
    run_id: String,
}

#[async_trait]
impl RunWriter for MemoryRun {
    fn info(&self) -> Run {
        self.state.lock().runs[&self.run_id].clone()
    }

    fn track(&self, name: &str, value: f64, opts: TrackOpts) -> Result<()> {
        let context = opts
            .context
            .unwrap_or_else(|| Value::Object(Default::default()));
        if !context.is_object() {
            return Err(Error::InvalidArgument(
                "a context must be a JSON object".into(),
            ));
        }
        let id = series_hash(&self.run_id, name, &context);
        let mut s = self.state.lock();
        s.series.entry(id).or_insert_with(|| Series {
            id,
            run_id: self.run_id.clone(),
            name: name.to_string(),
            context,
        });
        let pts = s.points.entry(id).or_default();
        let step = match opts.step {
            Some(step) => step,
            None => match pts.keys().next_back() {
                None => 0,
                Some(last) => last.checked_add(1).ok_or_else(|| {
                    Error::InvalidArgument(format!(
                        "{name}: no step after {last}; pass step explicitly"
                    ))
                })?,
            },
        };
        pts.insert(
            step,
            (value, opts.epoch, opts.timestamp.unwrap_or_else(now_micros)),
        );
        Ok(())
    }

    fn set_params(&self, params: &Value) -> Result<()> {
        if !params.is_object() {
            return Err(Error::InvalidArgument(
                "parameters must be a JSON object".into(),
            ));
        }
        let mut s = self.state.lock();
        let p = s
            .params
            .entry(self.run_id.clone())
            .or_insert_with(|| Value::Object(Default::default()));
        merge_json(p, params);
        Ok(())
    }

    fn set_param(&self, key: &str, value: Value) -> Result<()> {
        self.set_params(&nest(key, value))
    }

    async fn update(&self, patch: RunPatch) -> Result<()> {
        let mut s = self.state.lock();
        let r = s
            .runs
            .get_mut(&self.run_id)
            .ok_or_else(|| Error::NotFound(self.run_id.clone()))?;
        patch.apply(r);
        r.updated_at = now_micros().max(r.updated_at + 1);
        Ok(())
    }

    async fn finish(&self, status: RunStatus, ended_at: Option<i64>) -> Result<()> {
        self.update(RunPatch {
            status: Some(status),
            ended_at: Some(ended_at.unwrap_or_else(now_micros)),
            ..Default::default()
        })
        .await
    }

    async fn flush(&self) -> Result<()> {
        Ok(())
    }

    fn heartbeat(&self) -> Result<()> {
        let mut st = self.state.lock();
        let r = st
            .runs
            .get_mut(&self.run_id)
            .ok_or_else(|| Error::NotFound(format!("run {}", self.run_id)))?;
        r.updated_at = now_micros();
        Ok(())
    }
}
