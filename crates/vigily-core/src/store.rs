//! The storage interface. vigily-core talks to storage only through these traits, so the query,
//! catalog and chart code can be tested against [`crate::MemoryStore`] and the storage engine can
//! change without touching them. [`crate::Kv0Store`] is the production implementation.

use std::collections::HashMap;

use async_trait::async_trait;
use serde_json::Value;

use crate::error::{Error, Result};
use crate::model::{
    NewRun, Points, Run, RunPatch, RunStatus, Series, StepRange, Summary, TimeRange, TrackOpts,
};

pub use vigily_api::runs::{PurgeSummary, SeriesPoints};

/// A repository of runs.
#[async_trait]
pub trait Store: Send + Sync + 'static {
    /// Where the repository lives, for display.
    fn location(&self) -> String;

    /// Every run, newest first.
    async fn runs(&self) -> Result<Vec<Run>>;

    /// One run's parameters as a nested JSON object.
    async fn params(&self, run_id: &str) -> Result<Value>;

    /// One run's metric series.
    async fn series(&self, run_id: &str) -> Result<Vec<Series>>;

    /// Points of several series in a step range, keyed by series id. Series without points in the
    /// range may be missing from the result.
    async fn points(&self, series_ids: &[u64], range: StepRange) -> Result<HashMap<u64, Points>>;

    /// Points in a step range whose timestamps fall in `time`. The default filters [`Store::points`].
    async fn points_in_time(
        &self,
        series_ids: &[u64],
        range: StepRange,
        time: TimeRange,
    ) -> Result<HashMap<u64, Points>> {
        let mut out = self.points(series_ids, range).await?;
        for p in out.values_mut() {
            p.retain_time(time);
        }
        out.retain(|_, p| !p.is_empty());
        Ok(out)
    }

    /// Every run's parameters, keyed by run id. Backends override this with one read.
    async fn all_params(&self) -> Result<HashMap<String, Value>> {
        let mut out = HashMap::new();
        for r in self.runs().await? {
            let p = self.params(&r.id).await?;
            out.insert(r.id, p);
        }
        Ok(out)
    }

    /// Every series of every run. Backends override this with one read.
    async fn all_series(&self) -> Result<Vec<Series>> {
        let mut out = Vec::new();
        for r in self.runs().await? {
            out.extend(self.series(&r.id).await?);
        }
        Ok(out)
    }

    /// Statistics of every series, keyed by series id. Backends that store them read no points;
    /// the default computes them from all points.
    async fn series_stats(&self) -> Result<HashMap<u64, Summary>> {
        let ids: Vec<u64> = self.all_series().await?.iter().map(|s| s.id).collect();
        let points = self.points(&ids, StepRange::default()).await?;
        Ok(points
            .into_iter()
            .map(|(id, p)| {
                let mut s = Summary::default();
                s.extend(&p);
                (id, s)
            })
            .collect())
    }

    /// The versions of the stored tables (runs, parameters, series, metrics), when the backend
    /// is versioned. A reader skips the work for a table whose version did not change.
    async fn versions(&self) -> Result<Option<[u64; 4]>> {
        Ok(None)
    }

    /// Deletes runs with their parameters, series, points and per-run records. Returns how many
    /// of them existed.
    async fn delete_runs(&self, run_ids: &[String]) -> Result<usize>;

    /// Erases deleted runs from storage: compacts (dropping superseded and deleted record rows),
    /// rewrites the files that still hold deleted rows and removes every older version but tagged
    /// ones. Fails, naming the tag, when a tag keeps deleted data;
    /// with `force` such tags are deleted. Stores without history have nothing to purge.
    async fn purge(&self, _force: bool) -> Result<PurgeSummary> {
        Ok(PurgeSummary::default())
    }

    /// Starts a run in state running.
    async fn create_run(&self, new: NewRun) -> Result<Box<dyn RunWriter>>;

    /// Opens an existing run to write more points; each series continues after its last step.
    async fn open_run(&self, run_id: &str) -> Result<Box<dyn RunWriter>>;

    /// Changes a run's metadata and commits the change.
    async fn update_run(&self, run_id: &str, patch: RunPatch) -> Result<Run>;

    /// Commits buffered writes.
    async fn flush(&self) -> Result<()>;

    /// Compacts storage and builds indices. A no-op where it means nothing.
    async fn optimize(&self) -> Result<()> {
        Ok(())
    }

    /// Flushes and closes the repository.
    async fn close(&self) -> Result<()>;
}

/// Checks that the columns of [`RunWriter::track_batch`] line up; returns the number of points.
pub(crate) fn check_columns(points: &SeriesPoints) -> Result<usize> {
    let n = points.steps.len();
    if points.values.len() != n
        || !(points.epochs.is_empty() || points.epochs.len() == n)
        || !(points.timestamps.is_empty() || points.timestamps.len() == n)
    {
        return Err(Error::InvalidArgument(format!(
            "series {:?}: steps, values, epochs and timestamps must have the same length \
             (epochs and timestamps may be empty)",
            points.name
        )));
    }
    Ok(n)
}

/// The context of a [`RunWriter::track_batch`] series; `None` for the empty one.
pub(crate) fn batch_context(points: &SeriesPoints) -> Option<Value> {
    match &points.context {
        Value::Null => None,
        c => Some(c.clone()),
    }
}

/// A run being written.
#[async_trait]
pub trait RunWriter: Send + Sync {
    /// The run's metadata as this handle knows it.
    fn info(&self) -> Run;

    /// Records one metric point.
    fn track(&self, name: &str, value: f64, opts: TrackOpts) -> Result<()>;

    /// Records the points of one series given in columns: `steps` and `values` of the same
    /// length, `epochs` and `timestamps` of that length or empty (no epochs; now). `key` and
    /// `run_id` are ignored: the points belong to this run. A point replaces the one at the same
    /// step. The default calls [`RunWriter::track`] per point; the kv0 store writes the batch at
    /// once (`Run::track_many`).
    fn track_batch(&self, points: &SeriesPoints) -> Result<()> {
        let n = check_columns(points)?;
        let context = batch_context(points);
        for i in 0..n {
            self.track(
                &points.name,
                points.values[i],
                TrackOpts {
                    step: Some(points.steps[i]),
                    epoch: points.epochs.get(i).copied().flatten(),
                    context: context.clone(),
                    timestamp: points.timestamps.get(i).copied(),
                },
            )?;
        }
        Ok(())
    }

    /// Merges a JSON object into the parameters by dotted key.
    fn set_params(&self, params: &Value) -> Result<()>;

    /// Sets one parameter; the key may be dotted (`optimizer.lr`).
    fn set_param(&self, key: &str, value: Value) -> Result<()>;

    /// Changes the run's metadata. Implementations store only the fields `patch` changes, so edits
    /// made elsewhere (the web app) are kept.
    async fn update(&self, patch: RunPatch) -> Result<()>;

    /// Ends the run with `status` at `ended_at` (now when `None`) and commits everything. Like
    /// `update`, it changes only the status and end time of the stored record.
    async fn finish(&self, status: RunStatus, ended_at: Option<i64>) -> Result<()>;

    /// Commits buffered writes.
    async fn flush(&self) -> Result<()>;

    /// Records that the process writing this run is alive (see [`STALE_AFTER`]). The SDK calls it
    /// every 2 minutes while a run is open; it changes no field.
    fn heartbeat(&self) -> Result<()> {
        Ok(())
    }
}

/// A running run with no sign of life (heartbeat, metadata write or point) for this long is shown
/// as stale: its process most likely died. Microseconds.
pub const STALE_AFTER: i64 = 10 * 60 * 1_000_000;

/// The current time in microseconds since the Unix epoch.
pub fn now_micros() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or_default()
}
