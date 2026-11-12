//! [`Store`] on kv0's tracking layer (Lance datasets on a local directory or an object store).

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use kv0::tracking as lw;
use serde_json::Value;

use crate::error::Result;
use crate::model::{
    normalize_kind, normalize_target, NewRun, Points, Run, RunPatch, RunStatus, Series, StepRange,
    Summary, TimeRange, TrackOpts,
};
use crate::store::{batch_context, check_columns, RunWriter, SeriesPoints, Store};

/// Options for [`Kv0Store::open`].
#[derive(Clone, Debug)]
pub struct Kv0Options {
    /// Create the repository when it does not exist.
    pub create_if_missing: bool,
    /// Object-store options passed to Lance (`aws_region`, `aws_endpoint`, ...).
    pub storage_options: HashMap<String, String>,
    /// Commit buffered writes after this long.
    pub flush_interval: Duration,
}

impl Default for Kv0Options {
    fn default() -> Self {
        Self {
            create_if_missing: true,
            storage_options: HashMap::new(),
            flush_interval: Duration::from_secs(2),
        }
    }
}

/// A vigily repository stored with kv0.
#[derive(Clone, Debug)]
pub struct Kv0Store {
    repo: lw::Repo,
}

impl Kv0Store {
    /// Opens (or creates) the repository at `location`: a directory or a URI such as `s3://bucket/prefix`.
    pub async fn open(location: &str, opts: Kv0Options) -> Result<Self> {
        let mut o = lw::RepoOptions {
            create_if_missing: opts.create_if_missing,
            flush_interval: Some(opts.flush_interval),
            // `open_store` migrates the shared records table of earlier builds before this open
            // (`records::Kv0Records::open`) and keeps it for the operator to delete.
            legacy_records: lw::LegacyRecords::Allow,
            ..Default::default()
        };
        for (k, v) in &opts.storage_options {
            o.storage_options.insert(k.clone(), v.clone());
        }
        let repo = lw::Repo::open(location, o).await?;
        Ok(Self { repo })
    }

    pub fn inner(&self) -> &lw::Repo {
        &self.repo
    }
}

fn status_to(s: RunStatus) -> lw::RunStatus {
    match s {
        RunStatus::Running => lw::RunStatus::Running,
        RunStatus::Finished => lw::RunStatus::Finished,
        RunStatus::Failed => lw::RunStatus::Failed,
        RunStatus::Aborted => lw::RunStatus::Aborted,
    }
}

fn status_from(s: lw::RunStatus) -> RunStatus {
    match s {
        lw::RunStatus::Running => RunStatus::Running,
        lw::RunStatus::Finished => RunStatus::Finished,
        lw::RunStatus::Failed => RunStatus::Failed,
        lw::RunStatus::Aborted => RunStatus::Aborted,
    }
}

fn run_from(i: lw::RunInfo) -> Run {
    Run {
        id: i.run_id,
        name: i.name,
        experiment: i.experiment,
        description: i.description,
        tags: i.tags,
        status: status_from(i.status),
        archived: i.archived,
        created_at: i.created_at,
        updated_at: i.updated_at,
        ended_at: i.ended_at,
        kind: i.kind,
        target: i.target,
    }
}

fn series_from(s: lw::SeriesInfo) -> Series {
    Series {
        id: s.series_id,
        run_id: s.run_id,
        name: s.name,
        context: s.context,
    }
}

fn summary_from(s: lw::SeriesStats) -> Summary {
    Summary {
        count: s.count,
        non_finite: s.non_finite,
        first_step: Some(s.first_step),
        last_step: Some(s.last_step),
        last: Some(s.last),
        min: s.min,
        min_step: s.min_step,
        max: s.max,
        max_step: s.max_step,
        last_epoch: None,
        first_time: None,
        last_time: Some(s.last_timestamp),
    }
}

fn points_from(d: lw::MetricData) -> Points {
    Points {
        steps: d.steps,
        values: d.values,
        epochs: d.epochs,
        timestamps: d.timestamps,
    }
}

/// Applies a vigily patch to a kv0 record. kv0 stores only the fields that changed.
fn patch_info(patch: &RunPatch, info: &mut lw::RunInfo) {
    let mut r = run_from(info.clone());
    patch.apply(&mut r);
    info.name = r.name;
    info.experiment = r.experiment;
    info.description = r.description;
    info.tags = r.tags;
    info.status = status_to(r.status);
    info.archived = r.archived;
    info.created_at = r.created_at;
    info.ended_at = r.ended_at;
}

fn range_to(r: StepRange) -> lw::StepRange {
    lw::StepRange {
        start: r.start,
        stop: r.stop,
    }
}

#[async_trait]
impl Store for Kv0Store {
    fn location(&self) -> String {
        self.repo.uri().to_string()
    }

    async fn runs(&self) -> Result<Vec<Run>> {
        Ok(self.repo.runs().await?.into_iter().map(run_from).collect())
    }

    async fn params(&self, run_id: &str) -> Result<Value> {
        Ok(self.repo.params(run_id).await?)
    }

    async fn series(&self, run_id: &str) -> Result<Vec<Series>> {
        Ok(self
            .repo
            .series(run_id)
            .await?
            .into_iter()
            .map(series_from)
            .collect())
    }

    async fn points(&self, ids: &[u64], range: StepRange) -> Result<HashMap<u64, Points>> {
        Ok(self
            .repo
            .read_many(ids, range_to(range))
            .await?
            .into_iter()
            .map(|(id, d)| (id, points_from(d)))
            .collect())
    }

    async fn points_in_time(
        &self,
        ids: &[u64],
        range: StepRange,
        time: TimeRange,
    ) -> Result<HashMap<u64, Points>> {
        let t = lw::TimeRange {
            start: time.start,
            stop: time.stop,
        };
        Ok(self
            .repo
            .read_many_in(ids, range_to(range), t)
            .await?
            .into_iter()
            .map(|(id, d)| (id, points_from(d)))
            .collect())
    }

    async fn all_params(&self) -> Result<HashMap<String, Value>> {
        Ok(self.repo.all_params().await?)
    }

    async fn all_series(&self) -> Result<Vec<Series>> {
        Ok(self
            .repo
            .all_series()
            .await?
            .into_iter()
            .map(series_from)
            .collect())
    }

    async fn series_stats(&self) -> Result<HashMap<u64, Summary>> {
        Ok(self
            .repo
            .all_series_stats()
            .await?
            .into_iter()
            .map(|(id, s)| (id, summary_from(s)))
            .collect())
    }

    async fn versions(&self) -> Result<Option<[u64; 4]>> {
        let (a, b, c, d) = self.repo.latest_versions().await?;
        Ok(Some([a, b, c, d]))
    }

    async fn delete_runs(&self, run_ids: &[String]) -> Result<usize> {
        Ok(self.repo.delete_runs(run_ids).await?)
    }

    async fn purge(&self, force: bool) -> Result<crate::store::PurgeSummary> {
        // Compacting first drops superseded and deleted record rows (traces deleted without a run,
        // rows moved under a run when a later batch named it), so the purge then erases them from
        // the files as well as the rows of deleted runs.
        self.repo.optimize().await?;
        let r = self.repo.purge(force).await?;
        Ok(crate::store::PurgeSummary {
            runs: r.runs,
            fragments_rewritten: r.fragments_rewritten,
            versions_removed: r.versions_removed,
            bytes_removed: r.bytes_removed,
            tags_deleted: r.tags_deleted,
            ..Default::default()
        })
    }

    async fn create_run(&self, new: NewRun) -> Result<Box<dyn RunWriter>> {
        let run = self.repo.create_run(lw::NewRun {
            run_id: new.id,
            name: new.name,
            experiment: new.experiment,
            description: new.description,
            tags: new.tags,
            created_at: new.created_at,
            kind: normalize_kind(new.kind.as_deref()),
            target: normalize_target(new.target.as_ref()),
        })?;
        Ok(Box::new(Kv0Run {
            repo: self.repo.clone(),
            run,
        }))
    }

    async fn open_run(&self, run_id: &str) -> Result<Box<dyn RunWriter>> {
        let run = self.repo.resume_run(run_id).await?;
        Ok(Box::new(Kv0Run {
            repo: self.repo.clone(),
            run,
        }))
    }

    async fn update_run(&self, run_id: &str, patch: RunPatch) -> Result<Run> {
        // kv0 records only the fields the patch changes and merges stored rows field by field,
        // so a concurrent write of other fields (a training process finishing the run) is kept.
        // No points are read.
        let info = self
            .repo
            .update_run(run_id, |i| patch_info(&patch, i))
            .await?;
        self.repo.flush().await?;
        Ok(run_from(info))
    }

    async fn flush(&self) -> Result<()> {
        Ok(self.repo.flush().await?)
    }

    async fn optimize(&self) -> Result<()> {
        Ok(self.repo.optimize().await?)
    }

    async fn close(&self) -> Result<()> {
        Ok(self.repo.close().await?)
    }
}

struct Kv0Run {
    repo: lw::Repo,
    run: lw::Run,
}

#[async_trait]
impl RunWriter for Kv0Run {
    fn info(&self) -> Run {
        run_from(self.run.info())
    }

    fn track(&self, name: &str, value: f64, opts: TrackOpts) -> Result<()> {
        Ok(self.run.track(
            name,
            value,
            lw::Track {
                step: opts.step,
                epoch: opts.epoch,
                context: opts.context,
                timestamp: opts.timestamp,
            },
        )?)
    }

    fn track_batch(&self, points: &SeriesPoints) -> Result<()> {
        let n = check_columns(points)?;
        let batch: Vec<lw::TrackPoint> = (0..n)
            .map(|i| lw::TrackPoint {
                value: points.values[i],
                step: Some(points.steps[i]),
                epoch: points.epochs.get(i).copied().flatten(),
                timestamp: points.timestamps.get(i).copied(),
            })
            .collect();
        Ok(self
            .run
            .track_many(&points.name, batch_context(points), &batch)?)
    }

    fn set_params(&self, params: &Value) -> Result<()> {
        Ok(self.run.set_params(params)?)
    }

    fn set_param(&self, key: &str, value: Value) -> Result<()> {
        Ok(self.run.set_param(key, value)?)
    }

    async fn update(&self, patch: RunPatch) -> Result<()> {
        // Only the fields the patch changes are stored; edits made elsewhere to other fields stay.
        Ok(self.run.update(|i| patch_info(&patch, i))?)
    }

    async fn finish(&self, status: RunStatus, ended_at: Option<i64>) -> Result<()> {
        Ok(self.run.finish_at(status_to(status), ended_at).await?)
    }

    fn heartbeat(&self) -> Result<()> {
        // A change of no field: kv0 stores a row that only moves `updated_at`, which reads take
        // as the newest of all rows, so it overwrites nothing.
        Ok(self.run.update(|_| {})?)
    }

    async fn flush(&self) -> Result<()> {
        Ok(self.repo.flush().await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;

    #[tokio::test(flavor = "multi_thread")]
    async fn round_trip_through_lance() {
        let dir = tempfile::tempdir().unwrap();
        let loc = dir.path().join("repo");
        let store = Kv0Store::open(loc.to_str().unwrap(), Kv0Options::default())
            .await
            .unwrap();
        let run = store
            .create_run(NewRun {
                name: Some("baseline".into()),
                experiment: Some("mnist".into()),
                created_at: Some(1_000_000),
                ..Default::default()
            })
            .await
            .unwrap();
        run.set_params(&json!({"lr": 0.01, "opt": {"name": "adam"}}))
            .unwrap();
        for i in 0..100 {
            run.track(
                "loss",
                1.0 / (i as f64 + 1.0),
                TrackOpts {
                    context: Some(json!({"subset": "train"})),
                    ..Default::default()
                },
            )
            .unwrap();
        }
        run.finish(RunStatus::Finished, Some(9_000_000))
            .await
            .unwrap();
        let id = run.info().id;

        let runs = store.runs().await.unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].created_at, 1_000_000);
        assert_eq!(runs[0].ended_at, Some(9_000_000));
        assert_eq!(runs[0].status, RunStatus::Finished);
        assert_eq!(
            store.params(&id).await.unwrap()["opt"]["name"],
            json!("adam")
        );
        let series = store.series(&id).await.unwrap();
        assert_eq!(series.len(), 1);
        assert_eq!(series[0].key(), "loss[train]");
        let pts = store
            .points(
                &[series[0].id],
                StepRange {
                    start: Some(90),
                    stop: None,
                },
            )
            .await
            .unwrap();
        assert_eq!(pts[&series[0].id].steps, (90..100).collect::<Vec<_>>());

        let updated = store
            .update_run(
                &id,
                RunPatch {
                    description: Some("notes".into()),
                    tags: Some(vec!["best".into()]),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(updated.created_at, 1_000_000);
        let runs = store.runs().await.unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].description.as_deref(), Some("notes"));
        assert_eq!(runs[0].tags, vec!["best"]);
        assert_eq!(runs[0].status, RunStatus::Finished);
        assert_eq!(runs[0].created_at, 1_000_000);
        store.close().await.unwrap();
    }

    /// A batch goes to kv0 in one `track_many` call: later points of a step win, non-finite values
    /// and missing epochs and timestamps come through, and mismatched columns are refused whole.
    #[tokio::test(flavor = "multi_thread")]
    async fn track_batch_writes_columns() {
        let dir = tempfile::tempdir().unwrap();
        let loc = dir.path().join("repo");
        let store = Kv0Store::open(loc.to_str().unwrap(), Kv0Options::default())
            .await
            .unwrap();
        let run = store.create_run(NewRun::default()).await.unwrap();
        let batch = |steps: Vec<i64>, values: Vec<f64>| SeriesPoints {
            run_id: String::new(),
            name: "loss".into(),
            context: json!({"subset": "val"}),
            key: String::new(),
            steps,
            values,
            epochs: Vec::new(),
            timestamps: Vec::new(),
        };
        run.track_batch(&batch(vec![0, 1, 2], vec![1.0, f64::NAN, 3.0]))
            .unwrap();
        run.track_batch(&SeriesPoints {
            epochs: vec![Some(1), None],
            timestamps: vec![5, 6],
            ..batch(vec![2, 3], vec![f64::INFINITY, 4.0])
        })
        .unwrap();
        let err = run.track_batch(&batch(vec![4, 5], vec![5.0])).unwrap_err();
        assert!(err.to_string().contains("same length"), "{err}");
        run.flush().await.unwrap();

        let id = run.info().id;
        let series = store.series(&id).await.unwrap();
        assert_eq!(series.len(), 1);
        assert_eq!(series[0].key(), "loss[val]");
        let p = &store
            .points(&[series[0].id], StepRange::default())
            .await
            .unwrap()[&series[0].id];
        assert_eq!(p.steps, vec![0, 1, 2, 3]);
        assert_eq!(p.values[0], 1.0);
        assert!(p.values[1].is_nan());
        assert_eq!(p.values[2], f64::INFINITY);
        assert_eq!(p.epochs, vec![None, None, Some(1), None]);
        assert_eq!(p.timestamps[2..], [5, 6]);
        store.close().await.unwrap();
    }

    /// A web edit made while a run is going survives the SDK finishing the run, and an edit made
    /// after it finished keeps the finished status (both sides re-read before writing).
    #[tokio::test(flavor = "multi_thread")]
    async fn web_edits_and_sdk_finish_do_not_overwrite_each_other() {
        let dir = tempfile::tempdir().unwrap();
        let loc = dir.path().join("repo");
        let loc = loc.to_str().unwrap();
        let sdk = Kv0Store::open(loc, Kv0Options::default()).await.unwrap();
        let server = Kv0Store::open(loc, Kv0Options::default()).await.unwrap();
        let w = sdk
            .create_run(NewRun {
                name: Some("run-a".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        let id = w.info().id;
        w.track("loss", 1.0, TrackOpts::default()).unwrap();
        w.flush().await.unwrap();
        let patch = RunPatch {
            name: Some("best-run".into()),
            tags: Some(vec!["keep".into()]),
            ..Default::default()
        };
        server.update_run(&id, patch).await.unwrap();
        w.finish(RunStatus::Finished, None).await.unwrap();
        let r = server
            .runs()
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.id == id)
            .unwrap();
        assert_eq!(r.name.as_deref(), Some("best-run"));
        assert_eq!(r.tags, vec!["keep"]);
        assert_eq!(r.status, RunStatus::Finished);
        server
            .update_run(
                &id,
                RunPatch {
                    description: Some("kept".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let r = sdk
            .runs()
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.id == id)
            .unwrap();
        assert_eq!(
            (r.status, r.description.as_deref(), r.name.as_deref()),
            (RunStatus::Finished, Some("kept"), Some("best-run"))
        );
        // An SDK rename keeps the web app's tags.
        w.update(RunPatch {
            name: Some("renamed".into()),
            ..Default::default()
        })
        .await
        .unwrap();
        w.flush().await.unwrap();
        let r = server
            .runs()
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.id == id)
            .unwrap();
        assert_eq!(
            (r.name.as_deref(), r.tags.clone()),
            (Some("renamed"), vec!["keep".to_string()])
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn catalog_reads_stored_stats_skips_unchanged_tables_and_sees_deletes() {
        let dir = tempfile::tempdir().unwrap();
        let loc = dir.path().join("repo");
        let store = Arc::new(
            Kv0Store::open(loc.to_str().unwrap(), Kv0Options::default())
                .await
                .unwrap(),
        );
        let mut ids = Vec::new();
        for i in 0..3 {
            let w = store
                .create_run(NewRun {
                    kind: (i == 2).then(|| "Eval".into()),
                    target: (i == 2).then(|| json!("endpoint-a")),
                    ..Default::default()
                })
                .await
                .unwrap();
            w.set_params(&json!({"lr": 0.1 * i as f64})).unwrap();
            for step in 0..100 {
                w.track(
                    "loss",
                    1.0 / (step + 1) as f64,
                    TrackOpts {
                        step: Some(step),
                        ..Default::default()
                    },
                )
                .unwrap();
            }
            w.finish(RunStatus::Finished, None).await.unwrap();
            ids.push(w.info().id);
        }
        // Background compaction commits new table versions; let it settle so the second
        // refresh below sees the same versions as the first.
        store.inner().wait_for_maintenance().await;
        let catalog = crate::Catalog::new(store.clone());
        let snap = catalog.refresh().await.unwrap();
        assert_eq!(snap.runs.len(), 3);
        let e = snap.get(&ids[2]).unwrap();
        assert_eq!(
            (e.kind(), e.target()),
            ("eval", json!({"ref": "endpoint-a"}))
        );
        let s = &e.series[0].summary;
        assert_eq!((s.count, s.last_step, s.min), (100, Some(99), Some(0.01)));

        // Nothing changed: the same entries are reused.
        let again = catalog.refresh().await.unwrap();
        assert!(Arc::ptr_eq(&snap.runs[0], &again.runs[0]));

        assert_eq!(store.delete_runs(&ids[..1]).await.unwrap(), 1);
        let after = catalog.refresh().await.unwrap();
        assert_eq!(after.runs.len(), 2);
        assert!(after.get(&ids[0]).is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn heartbeats_move_last_seen_and_keep_other_edits() {
        let dir = tempfile::tempdir().unwrap();
        let loc = dir.path().join("repo");
        let loc = loc.to_str().unwrap();
        let sdk = Kv0Store::open(loc, Kv0Options::default()).await.unwrap();
        let server = Kv0Store::open(loc, Kv0Options::default()).await.unwrap();
        let w = sdk.create_run(NewRun::default()).await.unwrap();
        let id = w.info().id;
        w.flush().await.unwrap();
        let first = server.runs().await.unwrap()[0].updated_at;
        server
            .update_run(
                &id,
                RunPatch {
                    description: Some("note".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        w.heartbeat().unwrap();
        w.flush().await.unwrap();
        let r = server.runs().await.unwrap().remove(0);
        assert!(r.updated_at > first);
        assert_eq!(r.description.as_deref(), Some("note"));
        assert_eq!(r.status, RunStatus::Running);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn heartbeat_rows_are_folded_by_optimize() {
        let dir = tempfile::tempdir().unwrap();
        let loc = dir.path().join("repo");
        let loc = loc.to_str().unwrap();
        let store = Kv0Store::open(loc, Kv0Options::default()).await.unwrap();
        let mut writers = Vec::new();
        for _ in 0..3 {
            writers.push(store.create_run(NewRun::default()).await.unwrap());
        }
        // An hour of heartbeats every 2 minutes for three open runs, each flushed.
        for _ in 0..30 {
            for w in &writers {
                w.heartbeat().unwrap();
            }
            store.flush().await.unwrap();
        }
        let rows = |loc: String| async move {
            lance::Dataset::open(&format!("{loc}/runs.lance"))
                .await
                .unwrap()
                .count_rows(None)
                .await
                .unwrap()
        };
        let before = rows(loc.to_string()).await;
        store.optimize().await.unwrap();
        let after = rows(loc.to_string()).await;
        eprintln!("run rows: {before} before optimize, {after} after");
        assert!(before >= 90, "{before} run rows before folding");
        assert!(after <= 6, "{after} run rows after folding (3 runs)");
        let runs = store.runs().await.unwrap();
        assert_eq!(runs.len(), 3);
        assert!(runs.iter().all(|r| r.status == RunStatus::Running));
    }
}
