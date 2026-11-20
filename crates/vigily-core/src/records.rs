//! Per-item records: datasets, evaluation results, labels, feature samples, schedule runs, alerts.
//!
//! Metric series are `(step, value)` pairs; evaluations need rows per dataset item. Those rows live
//! in a [`RecordStore`]: JSON values under ordered string keys, written in batches and read by key
//! or by prefix. The key layout is owned by [`crate::evals`]; nothing else builds keys.
//!
//! Two implementations:
//!
//! - [`MemoryRecords`] for tests.
//! - [`Kv0Records`] (feature `kv0`) on kv0's records table, next to the tracking tables, on any
//!   backend kv0 supports. Writes are appends, so many processes can write; each key is
//!   last-writer-wins, and reads are filtered scans (nothing is held in memory).

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::RwLock;
use serde_json::Value;

use crate::error::{Error, Result};

/// Ordered JSON records.
#[async_trait]
pub trait RecordStore: Send + Sync + 'static {
    /// Writes several records. Rows that share a key prefix (`res/`, `dsv/`, ...) become visible
    /// together; prefixes are written in the order they first appear, so put the rows that are
    /// referenced first and the rows that refer to them last.
    async fn put(&self, rows: Vec<(String, Value)>) -> Result<()>;

    /// One record.
    async fn get(&self, key: &str) -> Result<Option<Value>>;

    /// Every record whose key starts with `prefix`, in key order.
    async fn scan(&self, prefix: &str) -> Result<Vec<(String, Value)>>;

    /// Every record under `<prefix><run>/` for each of `runs` (a per-run key space such as
    /// `res/`), in key order. Backends with a run index read them in one query.
    async fn scan_runs(&self, prefix: &str, runs: &[&str]) -> Result<Vec<(String, Value)>> {
        let mut out = Vec::new();
        for run in runs {
            out.extend(self.scan(&format!("{prefix}{run}/")).await?);
        }
        Ok(out)
    }

    /// The keys that start with `prefix`, in order, without reading their values.
    async fn keys(&self, prefix: &str) -> Result<Vec<String>> {
        Ok(self
            .scan(prefix)
            .await?
            .into_iter()
            .map(|(k, _)| k)
            .collect())
    }

    /// Writes rows of a table whose keys do not name their run (traces): each row carries its
    /// run (or none) explicitly, so deleting the run deletes the row and
    /// [`scan_owned`](Self::scan_owned) finds it. A key must always be written with the same run;
    /// a null value deletes it.
    async fn put_owned(&self, _rows: Vec<(String, Option<String>, Value)>) -> Result<()> {
        Err(Error::InvalidArgument(
            "this record store does not keep rows owned by runs".into(),
        ))
    }

    /// Every record under `prefix` that was written with [`put_owned`](Self::put_owned) for one
    /// of `runs`, in key order. Backends with a run index read them in one query.
    async fn scan_owned(&self, _prefix: &str, _runs: &[&str]) -> Result<Vec<(String, Value)>> {
        Err(Error::InvalidArgument(
            "this record store does not keep rows owned by runs".into(),
        ))
    }

    /// Removes records.
    async fn delete(&self, keys: Vec<String>) -> Result<()>;

    /// Commits buffered writes to storage.
    async fn flush(&self) -> Result<()>;

    /// Picks up records committed by other processes.
    async fn refresh(&self) -> Result<()>;

    /// The current version of the storage table that holds `key`, when the backend is versioned.
    async fn storage_version(&self, _key: &str) -> Result<Option<u64>> {
        Ok(None)
    }

    /// Names the current version of the storage table that holds `key`, so version cleanup keeps
    /// it and it can be read back (used to pin the dataset versions that audit records cite). The
    /// tag keeps only that table. When the tag exists already (another process made it), it is left
    /// as it is. Returns the version the tag names; `None` when the backend is not versioned.
    async fn tag(&self, _key: &str, _name: &str) -> Result<Option<u64>> {
        Ok(None)
    }
}

/// Records in memory. Cloning shares the data.
#[derive(Clone, Default)]
pub struct MemoryRecords {
    map: Arc<RwLock<BTreeMap<String, Value>>>,
    /// The run of each key written with `put_owned`.
    owners: Arc<RwLock<BTreeMap<String, String>>>,
}

impl MemoryRecords {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl RecordStore for MemoryRecords {
    async fn put(&self, rows: Vec<(String, Value)>) -> Result<()> {
        let mut m = self.map.write();
        for (k, v) in rows {
            m.insert(k, v);
        }
        Ok(())
    }

    async fn get(&self, key: &str) -> Result<Option<Value>> {
        Ok(self.map.read().get(key).cloned())
    }

    async fn scan(&self, prefix: &str) -> Result<Vec<(String, Value)>> {
        Ok(self
            .map
            .read()
            .range(prefix.to_string()..)
            .take_while(|(k, _)| k.starts_with(prefix))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect())
    }

    async fn put_owned(&self, rows: Vec<(String, Option<String>, Value)>) -> Result<()> {
        let (mut m, mut o) = (self.map.write(), self.owners.write());
        for (k, run, v) in rows {
            if v.is_null() {
                m.remove(&k);
                o.remove(&k);
                continue;
            }
            match run {
                Some(r) => o.insert(k.clone(), r),
                None => o.remove(&k),
            };
            m.insert(k, v);
        }
        Ok(())
    }

    async fn scan_owned(&self, prefix: &str, runs: &[&str]) -> Result<Vec<(String, Value)>> {
        let rows = self.scan(prefix).await?;
        let o = self.owners.read();
        Ok(rows
            .into_iter()
            .filter(|(k, _)| o.get(k).is_some_and(|r| runs.contains(&r.as_str())))
            .collect())
    }

    async fn delete(&self, keys: Vec<String>) -> Result<()> {
        let (mut m, mut o) = (self.map.write(), self.owners.write());
        for k in keys {
            m.remove(&k);
            o.remove(&k);
        }
        Ok(())
    }

    async fn flush(&self) -> Result<()> {
        Ok(())
    }

    async fn refresh(&self) -> Result<()> {
        Ok(())
    }
}

#[cfg(feature = "kv0")]
pub use table::Kv0Records;

#[cfg(feature = "kv0")]
mod table {
    use super::*;
    use kv0::tracking::{self as lw, NewRecord, RecordQuery};

    /// Records in kv0's records tables, next to the tracking tables. Each table is its own
    /// table with its own versions and tags:
    ///
    /// - a dataset's items and versions (`dsi/<name>/...`, `dsv/<name>/...`) live in the table
    ///   `dataset.<name>` (only characters kv0 stores as they are), so tagging a dataset version keeps only that dataset's history;
    /// - every other key lives in the table named by its first segment (`res`, `lab`, `feat`,
    ///   `sch`, `schout`, `alert`, `ds`, ...).
    ///
    /// The whole key is the item id; keys under `res/`, `lab/` and `feat/` also carry their run id,
    /// so a run's records are read through kv0's run-id index and deleted with the run. The newest
    /// row of a key wins and a null row deletes it. Nothing is held in memory.
    #[derive(Clone)]
    pub struct Kv0Records {
        repo: lw::Repo,
    }

    impl Kv0Records {
        /// Opens (or creates) the records of the repository at `location`.
        pub async fn open(location: &str) -> Result<Self> {
            let opts = lw::RepoOptions {
                create_if_missing: true,
                // This handle writes no runs or points: records are committed by each append.
                flush_interval: None,
                wal: false,
                ..Default::default()
            };
            let repo = crate::kv0_store::open_repo(location, opts).await?;
            let me = Self { repo };
            // Every open completes tags that a dataset commit did not get to. A failure here is
            // reported, not fatal: the next open tries again.
            match crate::evals::tag_untagged_datasets(&me).await {
                Ok(0) => {}
                Ok(n) => tracing::info!(
                    versions = n,
                    "tagged dataset versions without a storage tag"
                ),
                Err(e) => {
                    tracing::warn!(error = %e, "could not tag dataset versions; the next open retries")
                }
            }
            Ok(me)
        }
    }

    /// The storage table of a key.
    pub(super) fn table_of(key: &str) -> Result<String> {
        let mut parts = key.splitn(3, '/');
        match (parts.next(), parts.next()) {
            (Some("dsi" | "dsv"), Some(name)) if !name.is_empty() => Ok(format!("dataset.{name}")),
            (Some(t), Some(_)) if !t.is_empty() => Ok(t.to_string()),
            _ => Err(Error::InvalidArgument(format!(
                "record key without a table: {key}"
            ))),
        }
    }

    /// The run a key belongs to, for the per-run tables.
    fn run_of(key: &str) -> Option<&str> {
        let (table, rest) = key.split_once('/')?;
        if !matches!(table, "res" | "lab" | "feat") {
            return None;
        }
        rest.split_once('/')
            .map(|(run, _)| run)
            .filter(|r| !r.is_empty())
    }

    /// The newest value of each key under `prefix`, without deleted keys, in key order.
    fn latest(rows: Vec<lw::Record>, prefix: &str) -> Vec<(String, Value)> {
        let mut m: BTreeMap<String, (u64, Value)> = BTreeMap::new();
        for r in rows {
            if !r.item_id.starts_with(prefix) {
                continue;
            }
            match m.get(&r.item_id) {
                Some((seq, _)) if *seq > r.seq => {}
                _ => {
                    m.insert(r.item_id, (r.seq, r.data));
                }
            }
        }
        m.into_iter()
            .filter(|(_, (_, v))| !v.is_null())
            .map(|(k, (_, v))| (k, v))
            .collect()
    }

    impl Kv0Records {
        /// Appends rows, one commit per table, in the order the tables first appear: callers
        /// put referenced rows (items, outputs) before the rows that point at them.
        async fn append(&self, rows: Vec<(String, Value)>) -> Result<()> {
            self.append_owned(
                rows.into_iter()
                    .map(|(k, v)| {
                        let run = run_of(&k).map(str::to_string);
                        (k, run, v)
                    })
                    .collect(),
            )
            .await
        }

        /// Appends rows with their run ids, one commit per table, in the order the tables first
        /// appear.
        async fn append_owned(&self, rows: Vec<(String, Option<String>, Value)>) -> Result<()> {
            let mut groups: Vec<(String, Vec<NewRecord>)> = Vec::new();
            for (k, run_id, v) in rows {
                let table = table_of(&k)?;
                let rec = NewRecord {
                    run_id,
                    item_id: k,
                    data: v,
                };
                match groups.iter_mut().find(|(t, _)| *t == table) {
                    Some((_, g)) => g.push(rec),
                    None => groups.push((table, vec![rec])),
                }
            }
            for (table, rows) in groups {
                self.repo.append_records(&table, rows).await?;
            }
            Ok(())
        }
    }

    #[async_trait]
    impl RecordStore for Kv0Records {
        async fn put(&self, rows: Vec<(String, Value)>) -> Result<()> {
            self.append(rows).await
        }

        async fn get(&self, key: &str) -> Result<Option<Value>> {
            let q = RecordQuery {
                run_id: run_of(key).map(str::to_string),
                item_id: Some(key.to_string()),
                ..Default::default()
            };
            let rows = self.repo.records(&table_of(key)?, q).await?;
            Ok(latest(rows, key).into_iter().next().map(|(_, v)| v))
        }

        async fn scan(&self, prefix: &str) -> Result<Vec<(String, Value)>> {
            let q = RecordQuery {
                run_id: run_of(prefix).map(str::to_string),
                ..Default::default()
            };
            let rows = self.repo.records(&table_of(prefix)?, q).await?;
            Ok(latest(rows, prefix))
        }

        async fn scan_runs(&self, prefix: &str, runs: &[&str]) -> Result<Vec<(String, Value)>> {
            if runs.is_empty() {
                return Ok(Vec::new());
            }
            let q = RecordQuery {
                run_ids: Some(runs.iter().map(|r| r.to_string()).collect()),
                ..Default::default()
            };
            let rows = self.repo.records(&table_of(prefix)?, q).await?;
            Ok(latest(rows, prefix))
        }

        async fn put_owned(&self, rows: Vec<(String, Option<String>, Value)>) -> Result<()> {
            self.append_owned(rows).await
        }

        async fn scan_owned(&self, prefix: &str, runs: &[&str]) -> Result<Vec<(String, Value)>> {
            if runs.is_empty() {
                return Ok(Vec::new());
            }
            let q = RecordQuery {
                run_ids: Some(runs.iter().map(|r| r.to_string()).collect()),
                ..Default::default()
            };
            let rows = self.repo.records(&table_of(prefix)?, q).await?;
            Ok(latest(rows, prefix))
        }

        async fn delete(&self, keys: Vec<String>) -> Result<()> {
            self.append(keys.into_iter().map(|k| (k, Value::Null)).collect())
                .await
        }

        async fn flush(&self) -> Result<()> {
            Ok(())
        }

        async fn refresh(&self) -> Result<()> {
            Ok(())
        }

        async fn storage_version(&self, key: &str) -> Result<Option<u64>> {
            Ok(Some(self.repo.records_version(&table_of(key)?).await?))
        }

        async fn tag(&self, key: &str, name: &str) -> Result<Option<u64>> {
            let table = table_of(key)?;
            let existing = |tags: Vec<(String, u64)>| {
                tags.into_iter().find(|(t, _)| t == name).map(|(_, v)| v)
            };
            if let Some(v) = existing(self.repo.record_tags(&table).await?) {
                return Ok(Some(v));
            }
            if let Err(e) = self.repo.tag_records(&table, name, None).await {
                // Another process may have made the same tag in between.
                return match existing(self.repo.record_tags(&table).await?) {
                    Some(v) => Ok(Some(v)),
                    None => Err(e.into()),
                };
            }
            Ok(existing(self.repo.record_tags(&table).await?))
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use serde_json::json;

        #[tokio::test(flavor = "multi_thread")]
        async fn records_round_trip_overwrite_delete_and_share_between_handles() {
            let dir = tempfile::tempdir().unwrap();
            let loc = dir.path().to_str().unwrap();
            let a = Kv0Records::open(loc).await.unwrap();
            a.put(vec![
                ("res/r1/b".into(), json!({"v": 2})),
                ("res/r1/a".into(), json!({"v": 1})),
                ("res/r2/a".into(), json!({"v": 3})),
                ("ds/qa".into(), json!({"latest": 1})),
                ("dsv/qa/00000001".into(), json!({"version": 1})),
            ])
            .await
            .unwrap();
            let rows = a.scan("res/r1/").await.unwrap();
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[0].0, "res/r1/a");
            a.put(vec![("ds/qa".into(), json!({"latest": 2}))])
                .await
                .unwrap();
            assert_eq!(a.get("ds/qa").await.unwrap(), Some(json!({"latest": 2})));
            assert_eq!(a.scan("ds/").await.unwrap().len(), 1);

            let b = Kv0Records::open(loc).await.unwrap();
            assert_eq!(b.get("res/r2/a").await.unwrap(), Some(json!({"v": 3})));
            a.delete(vec!["res/r2/a".into()]).await.unwrap();
            assert_eq!(b.get("res/r2/a").await.unwrap(), None);
            assert!(b.keys("res/r2/").await.unwrap().is_empty());
            assert!(a.get("nokey").await.is_err());

            // A dataset's tag keeps only that dataset's table.
            // The tag names the latest version at the time (making it may itself commit).
            let v = a.storage_version("dsv/qa/").await.unwrap().unwrap();
            assert_eq!(a.tag("dsv/qa/", "ds-qa-v1").await.unwrap(), Some(v));
            assert_eq!(
                a.repo.record_tags("dataset.qa").await.unwrap(),
                vec![("ds-qa-v1".to_string(), v)]
            );
            assert!(a.repo.record_tags("res").await.unwrap().is_empty());
            let tables = a.repo.record_tables().await.unwrap();
            assert!(
                tables.contains(&"dataset.qa".to_string()) && tables.contains(&"res".to_string())
            );
        }

        /// A dataset `qa` with one version, written as records (no storage tag yet).
        fn dataset_rows() -> Vec<(String, Value)> {
            vec![
                ("dsi/qa/h1".into(), json!({"input": "hi"})),
                (
                    "dsv/qa/00000001".into(),
                    json!({"name": "qa", "version": 1, "created_at": 1, "note": null,
                    "digest": "d", "items": [{"id": "a", "hash": "h1"}],
                    "changes": {"added": 1, "updated": 0, "removed": 0}, "source": "sdk",
                    "storage_version": null, "storage_tag": null}),
                ),
                (
                    "ds/qa".into(),
                    json!({"name": "qa", "description": null, "created_at": 1,
                        "updated_at": 1, "latest": 1, "items": 1}),
                ),
            ]
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn untagged_versions_are_tagged_on_open_and_existing_tags_are_kept() {
            let dir = tempfile::tempdir().unwrap();
            let loc = dir.path().to_str().unwrap();
            Kv0Records::open(loc)
                .await
                .unwrap()
                .put(dataset_rows())
                .await
                .unwrap();
            let rs = Kv0Records::open(loc).await.unwrap();
            let tagged = crate::evals::dataset_version(&rs, "qa", Some(1))
                .await
                .unwrap();
            assert_eq!(
                tagged.storage_tag.as_deref(),
                Some(crate::evals::dataset_tag("qa", 1).as_str())
            );
            // The tag exists in storage, but the version row lost it (a process stopped after
            // committing, or another process tagged it first).
            let mut v = tagged.clone();
            v.storage_tag = None;
            v.storage_version = None;
            rs.put(vec![(
                "dsv/qa/00000001".into(),
                serde_json::to_value(&v).unwrap(),
            )])
            .await
            .unwrap();
            let again = Kv0Records::open(loc).await.unwrap();
            let healed = crate::evals::dataset_version(&again, "qa", Some(1))
                .await
                .unwrap();
            assert_eq!(healed.storage_tag, tagged.storage_tag);
            assert_eq!(
                healed.storage_version, tagged.storage_version,
                "the existing tag is taken as it is"
            );
            assert_eq!(again.repo.record_tags("dataset.qa").await.unwrap().len(), 1);
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn a_repository_of_an_earlier_vigily_is_refused_untouched() {
            // The layout kv0 0.1.0-beta.2 and earlier wrote: Lance datasets named `<table>.lance`.
            let dir = tempfile::tempdir().unwrap();
            let loc = dir.path().to_str().unwrap();
            let runs = dir.path().join("runs.lance");
            std::fs::create_dir_all(runs.join("_versions")).unwrap();
            let before: Vec<_> = std::fs::read_dir(dir.path())
                .unwrap()
                .flatten()
                .map(|e| e.file_name())
                .collect();
            for e in [
                Kv0Records::open(loc).await.err().unwrap(),
                crate::Kv0Store::open(loc, Default::default())
                    .await
                    .err()
                    .unwrap(),
            ] {
                assert!(matches!(e, crate::Error::Unsupported(_)), "{e}");
                let m = e.to_string();
                assert!(
                    m.contains("earlier vigily") && m.contains("no migration"),
                    "{m}"
                );
            }
            let after: Vec<_> = std::fs::read_dir(dir.path())
                .unwrap()
                .flatten()
                .map(|e| e.file_name())
                .collect();
            assert_eq!(before, after, "nothing was written");
        }

        #[test]
        fn keys_map_to_tables() {
            assert_eq!(
                table_of("dsi/support-intents/ab12").unwrap(),
                "dataset.support-intents"
            );
            assert_eq!(table_of("dsv/qa/00000002").unwrap(), "dataset.qa");
            assert_eq!(table_of("ds/qa").unwrap(), "ds");
            assert_eq!(table_of("res/run1/item").unwrap(), "res");
            assert!(table_of("dsv/").is_err() || table_of("dsv/").unwrap() == "dsv");
        }
    }
}
