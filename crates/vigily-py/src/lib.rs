//! The native half of the `vigily` Python package (`vigily._native`). The Python half wraps it
//! in `vigily.Run` and `vigily.Repo`; values cross the boundary as JSON text.

use std::sync::Arc;

use pyo3::create_exception;
use pyo3::exceptions::{PyFileNotFoundError, PyLookupError, PyOSError, PyValueError};
use pyo3::prelude::*;
use serde_json::Value;
use vigily_core::evals::{self, CommitMode, EvalResult, FeatureSample, ItemInput};
use vigily_core::{
    block_on, open_records, open_store, NewRun, RecordStore, RunPatch, RunStatus, RunWriter, Store,
    TrackOpts,
};

#[cfg(target_os = "linux")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn err(e: vigily_core::Error) -> PyErr {
    match e {
        vigily_core::Error::NotFound(m) => PyFileNotFoundError::new_err(m),
        vigily_core::Error::InvalidArgument(m) => PyValueError::new_err(m),
        other => PyOSError::new_err(other.to_string()),
    }
}

create_exception!(
    _native,
    RunDeletedError,
    PyLookupError,
    "The run was deleted (in the web app or by another process); nothing more is recorded for it."
);

impl RunHandle {
    /// Errors from writing to this run: storage reports a deleted run as not found.
    fn err(&self, e: vigily_core::Error) -> PyErr {
        match e {
            vigily_core::Error::NotFound(_) => RunDeletedError::new_err(format!(
                "run {} was deleted (in the web app or by another process); nothing more is recorded for it",
                self.w.info().id
            )),
            other => err(other),
        }
    }
}

fn json(s: &str) -> PyResult<Value> {
    serde_json::from_str(s).map_err(|e| PyValueError::new_err(format!("not JSON: {e}")))
}

fn to_json<T: serde::Serialize>(v: &T) -> PyResult<String> {
    serde_json::to_string(v).map_err(|e| PyValueError::new_err(e.to_string()))
}

/// A repository handle.
#[pyclass(module = "vigily._native", frozen)]
struct Repo {
    store: Arc<dyn Store>,
    location: String,
    records: std::sync::OnceLock<Arc<dyn RecordStore>>,
}

impl Repo {
    fn records(&self, py: Python<'_>) -> PyResult<Arc<dyn RecordStore>> {
        if let Some(r) = self.records.get() {
            return Ok(r.clone());
        }
        let loc = self.location.clone();
        let r = py.detach(|| block_on(open_records(&loc))).map_err(err)?;
        Ok(self.records.get_or_init(|| r).clone())
    }
}

#[pymethods]
impl Repo {
    #[new]
    fn new(py: Python<'_>, location: String) -> PyResult<Self> {
        let store = py.detach(|| block_on(open_store(&location))).map_err(err)?;
        Ok(Self {
            store,
            location,
            records: std::sync::OnceLock::new(),
        })
    }

    #[getter]
    fn location(&self) -> String {
        self.store.location()
    }

    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (name=None, experiment=None, description=None, tags=Vec::new(), run_id=None, created_at=None, kind=None, target=None))]
    fn create_run(
        &self,
        py: Python<'_>,
        name: Option<String>,
        experiment: Option<String>,
        description: Option<String>,
        tags: Vec<String>,
        run_id: Option<String>,
        created_at: Option<i64>,
        kind: Option<String>,
        target: Option<&str>,
    ) -> PyResult<RunHandle> {
        let target = target.map(json).transpose()?;
        let store = self.store.clone();
        let w = py
            .detach(|| {
                block_on(store.create_run(NewRun {
                    id: run_id,
                    name,
                    experiment,
                    description,
                    tags,
                    created_at,
                    kind,
                    target,
                }))
            })
            .map_err(err)?;
        Ok(RunHandle { w: Arc::from(w) })
    }

    fn open_run(&self, py: Python<'_>, run_id: String) -> PyResult<RunHandle> {
        let store = self.store.clone();
        let w = py
            .detach(|| block_on(store.open_run(&run_id)))
            .map_err(err)?;
        Ok(RunHandle { w: Arc::from(w) })
    }

    /// Every run as JSON, newest first.
    fn runs(&self, py: Python<'_>) -> PyResult<String> {
        let store = self.store.clone();
        let runs = py.detach(|| block_on(store.runs())).map_err(err)?;
        to_json(&runs)
    }

    fn params(&self, py: Python<'_>, run_id: String) -> PyResult<String> {
        let store = self.store.clone();
        let p = py.detach(|| block_on(store.params(&run_id))).map_err(err)?;
        to_json(&p)
    }

    fn series(&self, py: Python<'_>, run_id: String) -> PyResult<String> {
        let store = self.store.clone();
        let s = py.detach(|| block_on(store.series(&run_id))).map_err(err)?;
        to_json(&s)
    }

    /// Points of one series: (steps, values, epochs, timestamps in microseconds), optionally
    /// only those with `start_time <= timestamp < stop_time` (microseconds, UTC).
    #[allow(clippy::type_complexity, clippy::too_many_arguments)]
    #[pyo3(signature = (run_id, name, context, start_time=None, stop_time=None))]
    fn points(
        &self,
        py: Python<'_>,
        run_id: String,
        name: String,
        context: &str,
        start_time: Option<i64>,
        stop_time: Option<i64>,
    ) -> PyResult<(Vec<i64>, Vec<f64>, Vec<Option<i64>>, Vec<i64>)> {
        let ctx = json(context)?;
        let store = self.store.clone();
        let p = py
            .detach(|| {
                block_on(async move {
                    let series = store.series(&run_id).await?;
                    let Some(s) = series.iter().find(|s| s.name == name && s.context == ctx) else {
                        return Ok(Default::default());
                    };
                    let time = vigily_core::TimeRange {
                        start: start_time,
                        stop: stop_time,
                    };
                    let mut m = store
                        .points_in_time(&[s.id], Default::default(), time)
                        .await?;
                    Ok::<_, vigily_core::Error>(m.remove(&s.id).unwrap_or_default())
                })
            })
            .map_err(err)?;
        Ok((p.steps, p.values, p.epochs, p.timestamps))
    }

    fn flush(&self, py: Python<'_>) -> PyResult<()> {
        let store = self.store.clone();
        py.detach(|| block_on(store.flush())).map_err(err)?;
        if let Some(r) = self.records.get().cloned() {
            py.detach(|| block_on(r.flush())).map_err(err)?;
        }
        Ok(())
    }

    /// Commits a dataset version from a JSON list of items; returns the version as JSON.
    #[pyo3(signature = (name, items, mode="replace", note=None, description=None, source=None))]
    #[allow(clippy::too_many_arguments)]
    fn commit_dataset(
        &self,
        py: Python<'_>,
        name: String,
        items: &str,
        mode: &str,
        note: Option<String>,
        description: Option<String>,
        source: Option<String>,
    ) -> PyResult<String> {
        let items: Vec<ItemInput> = serde_json::from_str(items)
            .map_err(|e| PyValueError::new_err(format!("invalid items: {e}")))?;
        let mode = match mode {
            "replace" => CommitMode::Replace,
            "upsert" => CommitMode::Upsert,
            m => {
                return Err(PyValueError::new_err(format!(
                    "unknown mode {m:?}; use replace or upsert"
                )))
            }
        };
        let rs = self.records(py)?;
        let v = py
            .detach(|| {
                block_on(async move {
                    evals::commit_dataset(&*rs, &name, items, mode, note, description, source).await
                })
            })
            .map_err(err)?;
        to_json(&v)
    }

    fn datasets(&self, py: Python<'_>) -> PyResult<String> {
        let rs = self.records(py)?;
        let v = py
            .detach(|| block_on(async move { evals::datasets(&*rs).await }))
            .map_err(err)?;
        to_json(&v)
    }

    #[pyo3(signature = (name, version=None))]
    fn dataset_version(
        &self,
        py: Python<'_>,
        name: String,
        version: Option<u32>,
    ) -> PyResult<String> {
        let rs = self.records(py)?;
        let v = py
            .detach(|| block_on(async move { evals::dataset_version(&*rs, &name, version).await }))
            .map_err(err)?;
        to_json(&v)
    }

    fn dataset_versions(&self, py: Python<'_>, name: String) -> PyResult<String> {
        let rs = self.records(py)?;
        let v = py
            .detach(|| block_on(async move { evals::dataset_versions(&*rs, &name).await }))
            .map_err(err)?;
        to_json(&v)
    }

    /// The items of a version as JSON.
    #[pyo3(signature = (name, version=None))]
    fn dataset_items(
        &self,
        py: Python<'_>,
        name: String,
        version: Option<u32>,
    ) -> PyResult<String> {
        let rs = self.records(py)?;
        let v = py
            .detach(|| {
                block_on(async move {
                    let v = evals::dataset_version(&*rs, &name, version).await?;
                    evals::dataset_items(&*rs, &v).await
                })
            })
            .map_err(err)?;
        to_json(&v)
    }

    /// Stores eval results given as a JSON list.
    fn put_results(&self, py: Python<'_>, results: &str) -> PyResult<()> {
        let results: Vec<EvalResult> = serde_json::from_str(results)
            .map_err(|e| PyValueError::new_err(format!("invalid results: {e}")))?;
        let rs = self.records(py)?;
        py.detach(|| block_on(async move { evals::put_results(&*rs, results).await }))
            .map_err(err)
    }

    /// Stores spans given as a JSON list straight into the repository, applying `config` (the
    /// `[tracing]` section as JSON: payload mode, prices). For importers and the demo; the SDK's
    /// tracer sends spans to the server, which applies the operator's policy.
    fn put_spans(&self, py: Python<'_>, spans: &str, config: &str) -> PyResult<String> {
        let spans: Vec<vigily_core::traces::SpanIn> = serde_json::from_str(spans)
            .map_err(|e| PyValueError::new_err(format!("invalid spans: {e}")))?;
        let cfg: vigily_core::traces::TracingConfig = serde_json::from_str(config)
            .map_err(|e| PyValueError::new_err(format!("invalid tracing config: {e}")))?;
        cfg.validate().map_err(PyValueError::new_err)?;
        let rs = self.records(py)?;
        let report = py
            .detach(|| {
                block_on(async move {
                    vigily_core::traces::ingest(&*rs, &cfg, spans, &|_: &str| None).await
                })
            })
            .map_err(err)?;
        to_json(&report)
    }

    fn results(&self, py: Python<'_>, run_id: String) -> PyResult<String> {
        let rs = self.records(py)?;
        let v = py
            .detach(|| block_on(async move { evals::results(&*rs, &run_id).await }))
            .map_err(err)?;
        to_json(&v)
    }

    /// Stores feature samples given as a JSON list.
    fn put_features(&self, py: Python<'_>, samples: &str) -> PyResult<()> {
        let samples: Vec<FeatureSample> = serde_json::from_str(samples)
            .map_err(|e| PyValueError::new_err(format!("invalid feature samples: {e}")))?;
        let rs = self.records(py)?;
        py.detach(|| block_on(async move { evals::put_features(&*rs, samples).await }))
            .map_err(err)
    }

    fn optimize(&self, py: Python<'_>) -> PyResult<()> {
        let store = self.store.clone();
        py.detach(|| block_on(store.optimize())).map_err(err)
    }

    /// Deletes runs that are not running, with their points and evaluation records. Refuses the
    /// whole call when one of them is running. Returns how many existed.
    fn delete_runs(&self, py: Python<'_>, run_ids: Vec<String>) -> PyResult<usize> {
        let store = self.store.clone();
        py.detach(|| {
            block_on(async move {
                let running: Vec<String> = store
                    .runs()
                    .await?
                    .into_iter()
                    .filter(|r| r.status == RunStatus::Running && run_ids.contains(&r.id))
                    .map(|r| r.id)
                    .collect();
                if !running.is_empty() {
                    return Err(vigily_core::Error::InvalidArgument(format!(
                        "still running, finish or abort first: {}",
                        running.join(", ")
                    )));
                }
                store.delete_runs(&run_ids).await
            })
        })
        .map_err(err)
    }

    fn close(&self, py: Python<'_>) -> PyResult<()> {
        let store = self.store.clone();
        py.detach(|| block_on(store.close())).map_err(err)
    }
}

/// A run being written.
#[pyclass(module = "vigily._native", frozen)]
struct RunHandle {
    w: Arc<dyn RunWriter>,
}

#[pymethods]
impl RunHandle {
    #[getter]
    fn id(&self) -> String {
        self.w.info().id
    }

    /// The run's metadata as JSON.
    fn info(&self) -> PyResult<String> {
        to_json(&self.w.info())
    }

    #[pyo3(signature = (name, value, step=None, epoch=None, context=None, timestamp=None))]
    #[allow(clippy::too_many_arguments)]
    fn track(
        &self,
        py: Python<'_>,
        name: &str,
        value: f64,
        step: Option<i64>,
        epoch: Option<i64>,
        context: Option<&str>,
        timestamp: Option<i64>,
    ) -> PyResult<()> {
        let context = context.map(json).transpose()?;
        // A full buffer commits to storage inside track; never hold the GIL across that.
        py.detach(|| {
            self.w.track(
                name,
                value,
                TrackOpts {
                    step,
                    epoch,
                    context,
                    timestamp,
                },
            )
        })
        .map_err(|e| self.err(e))
    }

    fn set_params(&self, py: Python<'_>, params: &str) -> PyResult<()> {
        let v = json(params)?;
        py.detach(|| self.w.set_params(&v)).map_err(|e| self.err(e))
    }

    fn set_param(&self, py: Python<'_>, key: &str, value: &str) -> PyResult<()> {
        let v = json(value)?;
        py.detach(|| self.w.set_param(key, v))
            .map_err(|e| self.err(e))
    }

    /// Applies a JSON patch: name, experiment, description, tags, archived.
    fn update(&self, py: Python<'_>, patch: &str) -> PyResult<()> {
        let p: RunPatch = serde_json::from_str(patch)
            .map_err(|e| PyValueError::new_err(format!("invalid run change: {e}")))?;
        let w = self.w.clone();
        py.detach(|| block_on(w.update(p))).map_err(|e| self.err(e))
    }

    #[pyo3(signature = (status, ended_at=None))]
    fn finish(&self, py: Python<'_>, status: &str, ended_at: Option<i64>) -> PyResult<()> {
        let s = RunStatus::parse(status).ok_or_else(|| {
            PyValueError::new_err(format!(
                "unknown status {status:?}; use finished, failed or aborted"
            ))
        })?;
        let w = self.w.clone();
        py.detach(|| block_on(w.finish(s, ended_at)))
            .map_err(|e| self.err(e))
    }

    fn flush(&self, py: Python<'_>) -> PyResult<()> {
        let w = self.w.clone();
        py.detach(|| block_on(w.flush())).map_err(|e| self.err(e))
    }

    /// Records that this process is alive; the server shows runs without one for 10 minutes as stale.
    fn heartbeat(&self, py: Python<'_>) -> PyResult<()> {
        py.detach(|| self.w.heartbeat()).map_err(|e| self.err(e))
    }
}

#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Repo>()?;
    m.add_class::<RunHandle>()?;
    m.add("RunDeletedError", m.py().get_type::<RunDeletedError>())?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
