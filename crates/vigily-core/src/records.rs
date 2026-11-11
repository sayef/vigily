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

    /// Whether a records table of an earlier storage layout is still present (it is migrated on
    /// open but kept for the operator to delete).
    async fn legacy_present(&self) -> Result<bool> {
        Ok(false)
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

    /// Records in kv0's records tables (`<repo>/records/<table>.lance`), next to the tracking
    /// tables. Each table is its own dataset with its own versions and tags:
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
        /// Opens (or creates) the records of the repository at `location`, migrating the single
        /// shared records table of earlier builds first (see [`migrate_shared`]).
        pub async fn open(location: &str) -> Result<Self> {
            let opts = lw::RepoOptions {
                create_if_missing: true,
                // This handle writes no runs or points: records are committed by each append.
                flush_interval: None,
                wal: false,
                // vigily migrates the shared table itself, here, and keeps it for the operator.
                legacy_records: lw::LegacyRecords::Allow,
                ..Default::default()
            };
            let repo = lw::Repo::open(location, opts).await?;
            let me = Self { repo };
            migrate_shared(&me, location).await?;
            // Every open completes tags that a dataset commit or a migration did not get to. A
            // failure here is reported, not fatal: the next open tries again.
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

        async fn legacy_present(&self) -> Result<bool> {
            Ok(self.repo.has_legacy_records().await?)
        }
    }

    // ---------------------------------------------------------------------------------------------
    // Migration from the shared records table

    /// The marker row that says the shared table was migrated: table `meta`, this item id.
    const MIGRATED: &str = "meta/migrated-shared-records";

    /// Copies the records of the single shared table that earlier builds wrote
    /// (`<repo>/records.lance`, which kv0 no longer reads) into the per-table layout, once.
    ///
    /// - Crash-safe and idempotent: the newest value of each key is appended to its new table, the
    ///   dataset versions are tagged in their new tables, and the marker is written last. An
    ///   interrupted migration runs again from the start.
    /// - A key already present in the new layout is skipped, so a migration that runs again (or in a
    ///   second process) never overwrites a value written since.
    /// - On a local repository a lock file (`<repo>/.migrating`) keeps two processes from migrating
    ///   at once; on object storage, upgrade with one process (the storage guide says so).
    /// - The old table is never deleted here: remove it by hand once the migration is confirmed.
    pub(super) async fn migrate_shared(rs: &Kv0Records, location: &str) -> Result<usize> {
        if rs.get(MIGRATED).await?.is_some() {
            return Ok(0);
        }
        let Some(rows) = read_shared(rs).await? else {
            return Ok(0);
        };
        // Wait for any other process migrating this repository, then look again: it may have
        // finished (marker written) or failed (no marker, so this process migrates).
        let _lock = MigrationLock::acquire(location).await?;
        if rs.get(MIGRATED).await?.is_some() {
            return Ok(0);
        }
        // Newest value per key; keys deleted in the old table are left out.
        let mut newest: BTreeMap<String, (u64, Value)> = BTreeMap::new();
        for (key, seq, value) in rows {
            if newest.get(&key).is_none_or(|(s, _)| seq > *s) {
                newest.insert(key, (seq, value));
            }
        }
        // Keys the new layout has already (written since, or by an earlier attempt) stay as they are.
        let mut present: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut tables: Vec<String> = newest.keys().filter_map(|k| table_of(k).ok()).collect();
        tables.sort();
        tables.dedup();
        for table in &tables {
            for r in rs.repo.records(table, RecordQuery::default()).await? {
                present.insert(r.item_id);
            }
        }
        let rows: Vec<(String, Value)> = newest
            .into_iter()
            .filter(|(k, (_, v))| !v.is_null() && !present.contains(k))
            .map(|(k, (_, mut v))| {
                // A dataset version's storage version and tag named the shared table, which no
                // longer counts: drop them so the version is tagged again in its own table.
                if k.starts_with("dsv/") {
                    if let Some(o) = v.as_object_mut() {
                        o.insert("storage_version".into(), Value::Null);
                        o.insert("storage_tag".into(), Value::Null);
                    }
                }
                (k, v)
            })
            .collect();
        let n = rows.len();
        for chunk in rows.chunks(5_000) {
            rs.append(chunk.to_vec()).await?;
        }
        // Tag before the marker; versions this misses are tagged by a later open.
        if let Err(e) = crate::evals::tag_untagged_datasets(rs).await {
            tracing::warn!(error = %e, "could not tag migrated dataset versions; a later open retries");
        }
        let marker = serde_json::json!({
            "from": "records.lance",
            "rows": n,
            "at": crate::store::now_micros(),
        });
        rs.append(vec![(MIGRATED.to_string(), marker)]).await?;
        tracing::info!(
            rows = n,
            "migrated the shared records table to per-table records"
        );
        Ok(n)
    }

    /// An advisory lock (flock) on `<repo>/.migrating`, held while one process migrates, so a
    /// second process on the machine waits. The operating system releases it when its holder
    /// exits, crashed or not, so a dead holder never blocks anyone. The file itself stays; it is
    /// never deleted, since another process may hold a lock on it. Object stores get no lock (see
    /// [`migrate_shared`]).
    struct MigrationLock(#[allow(dead_code)] Option<std::fs::File>);

    impl MigrationLock {
        async fn acquire(location: &str) -> Result<Self> {
            if location.contains("://") {
                return Ok(Self(None));
            }
            let path = std::path::Path::new(location).join(".migrating");
            let file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(&path)
                .map_err(|e| Error::Storage(format!("{}: {e}", path.display())))?;
            let mut waited = false;
            loop {
                match file.try_lock() {
                    Ok(()) => return Ok(Self(Some(file))),
                    Err(std::fs::TryLockError::WouldBlock) => {
                        if !waited {
                            tracing::info!("another process is migrating the records; waiting");
                            waited = true;
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    }
                    Err(std::fs::TryLockError::Error(e)) => {
                        return Err(Error::Storage(format!("{}: {e}", path.display())))
                    }
                }
            }
        }
    }

    /// (key, seq, value) of every row of the shared table, or `None` when there is none.
    async fn read_shared(rs: &Kv0Records) -> Result<Option<Vec<(String, u64, Value)>>> {
        // One directory listing; the rows are read only when the table is there.
        if !rs.repo.has_legacy_records().await? {
            return Ok(None);
        }
        let rows = rs.repo.legacy_records(None).await?;
        if rows.is_empty() {
            return Ok(None);
        }
        Ok(Some(
            rows.into_iter()
                .map(|r| (r.item_id, r.seq, r.data))
                .collect(),
        ))
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
            a.tag("dsv/qa/", "ds-qa-v1").await.unwrap();
            let v = a.storage_version("dsv/qa/").await.unwrap().unwrap();
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

        /// (seq, table, run, key, value) of a row in the shared table.
        type SharedRow<'a> = (u64, &'a str, Option<&'a str>, &'a str, Option<Value>);

        /// Writes a shared records table the way earlier builds did (kv0 before per-table
        /// records: one `records.lance`, a deleted key stored as JSON `null`).
        async fn write_shared(loc: &str, rows: &[SharedRow<'_>]) {
            use arrow_array::{
                RecordBatch, RecordBatchIterator, StringArray, TimestampMicrosecondArray,
                UInt64Array,
            };
            use std::sync::Arc;
            let data: Vec<String> = rows
                .iter()
                .map(|r| r.4.clone().unwrap_or(Value::Null).to_string())
                .collect();
            let batch = RecordBatch::try_from_iter(vec![
                (
                    "seq",
                    Arc::new(UInt64Array::from_iter_values(rows.iter().map(|r| r.0))) as _,
                ),
                (
                    "table_name",
                    Arc::new(StringArray::from_iter_values(rows.iter().map(|r| r.1))) as _,
                ),
                (
                    "run_id",
                    Arc::new(rows.iter().map(|r| r.2).collect::<StringArray>()) as _,
                ),
                (
                    "item_id",
                    Arc::new(StringArray::from_iter_values(rows.iter().map(|r| r.3))) as _,
                ),
                ("data", Arc::new(StringArray::from_iter_values(data)) as _),
                (
                    "created_at",
                    Arc::new(
                        TimestampMicrosecondArray::from(vec![0; rows.len()]).with_timezone("UTC"),
                    ) as _,
                ),
            ])
            .unwrap();
            let schema = batch.schema();
            let reader = RecordBatchIterator::new(vec![Ok(batch)], schema);
            lance::Dataset::write(reader, &format!("{loc}/records.lance"), None)
                .await
                .unwrap();
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn the_shared_table_is_migrated_once_and_kept() {
            let dir = tempfile::tempdir().unwrap();
            let loc = dir.path().to_str().unwrap();
            write_shared(
                loc,
                &[
                    (1, "res", Some("r1"), "res/r1/a", Some(json!({"v": 1}))),
                    (2, "res", Some("r1"), "res/r1/a", Some(json!({"v": 2}))),
                    (3, "alert", None, "alert/x", Some(json!({"id": "x"}))),
                    (4, "alert", None, "alert/x", None),
                    (5, "dsi", None, "dsi/qa/h1", Some(json!({"input": "hi"}))),
                    (
                        6,
                        "dsv",
                        None,
                        "dsv/qa/00000001",
                        Some(
                            json!({"name": "qa", "version": 1, "created_at": 1, "note": null,
                            "digest": "d", "items": [{"id": "a", "hash": "h1"}],
                            "changes": {"added": 1, "updated": 0, "removed": 0}, "source": "sdk",
                            "storage_version": 7, "storage_tag": "old-shared-tag"}),
                        ),
                    ),
                    (
                        7,
                        "ds",
                        None,
                        "ds/qa",
                        Some(json!({"name": "qa", "description": null, "created_at": 1,
                            "updated_at": 1, "latest": 1, "items": 1})),
                    ),
                ],
            )
            .await;
            let rs = Kv0Records::open(loc).await.unwrap();
            assert_eq!(rs.get("res/r1/a").await.unwrap(), Some(json!({"v": 2})));
            assert_eq!(
                rs.get("alert/x").await.unwrap(),
                None,
                "deleted keys stay deleted"
            );
            assert_eq!(rs.scan("dsi/qa/").await.unwrap().len(), 1);
            assert!(rs.get(MIGRATED).await.unwrap().is_some());
            // Dataset versions are re-tagged in their own table.
            let v = crate::evals::dataset_version(&rs, "qa", Some(1))
                .await
                .unwrap();
            assert_eq!(
                v.storage_tag.as_deref(),
                Some(crate::evals::dataset_tag("qa", 1).as_str())
            );
            let tags = rs.repo.record_tags("dataset.qa").await.unwrap();
            assert_eq!(tags.len(), 1);
            // A second open does not migrate again, and the old table is kept.
            assert_eq!(migrate_shared(&rs, loc).await.unwrap(), 0);
            assert!(std::path::Path::new(&format!("{loc}/records.lance")).exists());
        }

        fn shared_rows() -> Vec<SharedRow<'static>> {
            vec![
                (
                    1,
                    "alert",
                    None,
                    "alert/a1",
                    Some(json!({"id": "a1", "acknowledged_at": null})),
                ),
                (2, "dsi", None, "dsi/qa/h1", Some(json!({"input": "hi"}))),
                (
                    3,
                    "dsv",
                    None,
                    "dsv/qa/00000001",
                    Some(
                        json!({"name": "qa", "version": 1, "created_at": 1, "note": null,
                        "digest": "d", "items": [{"id": "a", "hash": "h1"}],
                        "changes": {"added": 1, "updated": 0, "removed": 0}, "source": "sdk",
                        "storage_version": 7, "storage_tag": null}),
                    ),
                ),
                (
                    4,
                    "ds",
                    None,
                    "ds/qa",
                    Some(json!({"name": "qa", "description": null, "created_at": 1,
                        "updated_at": 1, "latest": 1, "items": 1})),
                ),
            ]
        }

        async fn raw(loc: &str) -> Kv0Records {
            let opts = lw::RepoOptions {
                legacy_records: lw::LegacyRecords::Allow,
                ..Default::default()
            };
            Kv0Records {
                repo: lw::Repo::open(loc, opts).await.unwrap(),
            }
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn a_migration_interrupted_after_copying_completes_on_the_next_open() {
            let dir = tempfile::tempdir().unwrap();
            let loc = dir.path().to_str().unwrap();
            write_shared(loc, &shared_rows()).await;
            // The first attempt copied some rows, then stopped: no tags, no marker.
            raw(loc)
                .await
                .append(vec![("dsi/qa/h1".into(), json!({"input": "hi"}))])
                .await
                .unwrap();
            let rs = Kv0Records::open(loc).await.unwrap();
            assert!(rs.get(MIGRATED).await.unwrap().is_some());
            assert!(rs.get("ds/qa").await.unwrap().is_some());
            let v = crate::evals::dataset_version(&rs, "qa", Some(1))
                .await
                .unwrap();
            assert_eq!(
                v.storage_tag.as_deref(),
                Some(crate::evals::dataset_tag("qa", 1).as_str())
            );
            // The lock file stays, but nobody holds it any more.
            let f = std::fs::File::open(format!("{loc}/.migrating")).unwrap();
            assert!(f.try_lock().is_ok());
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn untagged_versions_are_tagged_on_open_and_existing_tags_are_kept() {
            let dir = tempfile::tempdir().unwrap();
            let loc = dir.path().to_str().unwrap();
            write_shared(loc, &shared_rows()).await;
            let rs = Kv0Records::open(loc).await.unwrap();
            let tagged = crate::evals::dataset_version(&rs, "qa", Some(1))
                .await
                .unwrap();
            // The tag exists in storage, but the version row lost it (a process stopped after the
            // marker, or another process tagged it first).
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
        async fn a_waiter_migrates_when_the_lock_holder_fails_without_the_marker() {
            let dir = tempfile::tempdir().unwrap();
            let loc = dir.path().to_str().unwrap().to_string();
            write_shared(&loc, &shared_rows()).await;
            // Another process holds the lock, then dies without writing the marker.
            let holder = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(dir.path().join(".migrating"))
                .unwrap();
            holder.lock().unwrap();
            let l = loc.clone();
            let waiter = tokio::spawn(async move { Kv0Records::open(&l).await.unwrap() });
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            assert!(!waiter.is_finished(), "the waiter waits for the lock");
            drop(holder);
            let rs = waiter.await.unwrap();
            assert!(
                rs.get(MIGRATED).await.unwrap().is_some(),
                "the waiter migrated"
            );
            assert!(rs.get("ds/qa").await.unwrap().is_some());
        }

        #[tokio::test(flavor = "multi_thread")]
        async fn a_migration_never_overwrites_values_written_in_the_new_layout() {
            let dir = tempfile::tempdir().unwrap();
            let loc = dir.path().to_str().unwrap();
            write_shared(loc, &shared_rows()).await;
            // Another process migrated and then acknowledged the alert; this process's migration
            // runs late (no marker yet, as if both started at once).
            raw(loc)
                .await
                .append(vec![(
                    "alert/a1".into(),
                    json!({"id": "a1", "acknowledged_at": 5}),
                )])
                .await
                .unwrap();
            let rs = Kv0Records::open(loc).await.unwrap();
            assert_eq!(
                rs.get("alert/a1").await.unwrap(),
                Some(json!({"id": "a1", "acknowledged_at": 5}))
            );
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
