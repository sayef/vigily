//! Evaluation data on a [`RecordStore`]: datasets and their versions, per-item results and scores,
//! human labels, input feature samples, schedule runs, alerts and drift baselines.
//!
//! This module owns the key layout; every other part of vigily goes through these functions.
//!
//! | Key | Value |
//! |---|---|
//! | `ds/{name}` | [`DatasetInfo`] |
//! | `dsv/{name}/{version:08}` | [`DatasetVersion`]: the item ids and content hashes of one version |
//! | `dsi/{name}/{hash}` | [`Item`], shared by every version that contains it |
//! | `res/{run}/{item}` | [`EvalResult`] |
//! | `lab/{run}/{item}/{time:020}` | [`Label`] |
//! | `feat/{run}/{feature}` | [`FeatureSample`] |
//! | `sch/{schedule}/{started:020}-{attempt}` | [`ScheduleRun`] |
//! | `schstate/{schedule}` | [`ScheduleState`] |
//! | `alert/{created:020}-{hash}` | [`Alert`]; `alertkey/{dedupe}` points at it |
//! | `baseline/{suite}` | [`Baseline`] |
//!
//! Dataset versions are immutable and content-addressed: an item's hash covers its input, expected
//! output and metadata, and a version's digest covers its item ids and hashes in order. An eval run
//! records the dataset name, version and digest, so its results can always be traced to the exact
//! items it saw, and committing identical items twice does not create a new version.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::records::RecordStore;
use crate::store::now_micros;

pub use vigily_api::evals::{
    Alert, AlertKind, Baseline, BaselineMode, Changes, CommitMode, DatasetInfo, DatasetVersion,
    EvalResult, FeatureSample, Item, ItemInput, ItemRef, Label, ScheduleRun, ScheduleRunStatus,
    Score, Severity,
};

/// A short, stable content hash: the first 16 hex digits of SHA-256.
pub fn hash_hex(bytes: &[u8]) -> String {
    let d = Sha256::digest(bytes);
    d.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// JSON with object keys sorted, so equal values hash equally.
pub fn canonical_json(v: &Value) -> String {
    fn sort(v: &Value) -> Value {
        match v {
            Value::Object(m) => {
                let sorted: BTreeMap<&String, Value> =
                    m.iter().map(|(k, x)| (k, sort(x))).collect();
                Value::Object(sorted.into_iter().map(|(k, x)| (k.clone(), x)).collect())
            }
            Value::Array(a) => Value::Array(a.iter().map(sort).collect()),
            x => x.clone(),
        }
    }
    sort(v).to_string()
}

fn check_name(kind: &str, name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 128
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if ok {
        Ok(())
    } else {
        Err(Error::InvalidArgument(format!(
            "{kind} name {name:?} must be 1 to 128 letters, digits, '-', '_' or '.'"
        )))
    }
}

fn from_value<T: for<'de> Deserialize<'de>>(v: Value) -> Result<T> {
    serde_json::from_value(v).map_err(|e| Error::Storage(format!("bad record: {e}")))
}

fn to_value<T: Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

// ---------------------------------------------------------------------------------------------
// Datasets

fn new_item(i: ItemInput) -> Item {
    let content =
        serde_json::json!({"input": i.input, "expected": i.expected, "metadata": i.metadata});
    let hash = hash_hex(canonical_json(&content).as_bytes());
    let id =
        i.id.filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| hash_hex(canonical_json(&i.input).as_bytes())[..12].to_string());
    Item {
        id,
        hash,
        input: i.input,
        expected: i.expected,
        metadata: i.metadata,
    }
}

/// The storage tag of a dataset version: `ds-<name>-<hash>-v<version>`, with the name reduced to
/// the characters tags allow and a short hash of the full name so different names never collide.
pub fn dataset_tag(name: &str, version: u32) -> String {
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(48)
        .collect();
    format!("ds-{safe}-{}-v{version}", &hash_hex(name.as_bytes())[..6])
}

fn digest_of(items: &[ItemRef]) -> String {
    let mut h = Sha256::new();
    for i in items {
        h.update(i.id.as_bytes());
        h.update([0]);
        h.update(i.hash.as_bytes());
        h.update([1]);
    }
    h.finalize()
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Commits a new version of dataset `name`. Returns the latest version unchanged when the items
/// are identical to it.
pub async fn commit_dataset(
    rs: &dyn RecordStore,
    name: &str,
    items: Vec<ItemInput>,
    mode: CommitMode,
    note: Option<String>,
    description: Option<String>,
    source: Option<String>,
) -> Result<DatasetVersion> {
    check_name("dataset", name)?;
    let now = now_micros();
    let info: Option<DatasetInfo> = match rs.get(&format!("ds/{name}")).await? {
        Some(v) => Some(from_value(v)?),
        None => None,
    };
    let prev = match &info {
        Some(i) => Some(dataset_version(rs, name, Some(i.latest)).await?),
        None => None,
    };
    let new_items: Vec<Item> = items.into_iter().map(new_item).collect();
    let mut seen = std::collections::HashSet::new();
    for i in &new_items {
        if !seen.insert(i.id.clone()) {
            return Err(Error::InvalidArgument(format!(
                "item id {:?} appears twice; ids must be unique within a dataset",
                i.id
            )));
        }
    }
    // The new ordered list of refs.
    let mut refs: Vec<ItemRef> = match (mode, &prev) {
        (CommitMode::Upsert, Some(p)) => p.items.clone(),
        _ => Vec::new(),
    };
    let mut pos: BTreeMap<String, usize> = refs
        .iter()
        .enumerate()
        .map(|(i, r)| (r.id.clone(), i))
        .collect();
    for it in &new_items {
        let r = ItemRef {
            id: it.id.clone(),
            hash: it.hash.clone(),
        };
        match pos.get(&it.id) {
            Some(i) => refs[*i] = r,
            None => {
                pos.insert(it.id.clone(), refs.len());
                refs.push(r);
            }
        }
    }
    let digest = digest_of(&refs);
    if let Some(p) = &prev {
        if p.digest == digest {
            // The same content again. A commit whose tagging step failed is completed here, so a
            // retry heals it.
            let mut p = p.clone();
            if p.storage_tag.is_none() {
                tag_version(rs, &mut p).await?;
            }
            return Ok(p);
        }
    }
    let old: BTreeMap<&str, &str> = prev
        .as_ref()
        .map(|p| {
            p.items
                .iter()
                .map(|r| (r.id.as_str(), r.hash.as_str()))
                .collect()
        })
        .unwrap_or_default();
    let mut changes = Changes::default();
    for r in &refs {
        match old.get(r.id.as_str()) {
            None => changes.added += 1,
            Some(h) if *h != r.hash => changes.updated += 1,
            _ => {}
        }
    }
    let ids: std::collections::HashSet<&str> = refs.iter().map(|r| r.id.as_str()).collect();
    changes.removed = old.keys().filter(|k| !ids.contains(*k)).count();

    let version = prev.as_ref().map_or(1, |p| p.version + 1);
    let mut rows: Vec<(String, Value)> = new_items
        .iter()
        .filter(|i| old.get(i.id.as_str()) != Some(&i.hash.as_str()))
        .map(|i| (format!("dsi/{name}/{}", i.hash), to_value(i)))
        .collect();
    let mut v = DatasetVersion {
        name: name.to_string(),
        version,
        created_at: now,
        note,
        digest,
        items: refs,
        changes,
        source,
        storage_version: None,
        storage_tag: None,
    };
    rows.push((format!("dsv/{name}/{version:08}"), to_value(&v)));
    let info = DatasetInfo {
        name: name.to_string(),
        description: description.or_else(|| info.as_ref().and_then(|i| i.description.clone())),
        created_at: info.as_ref().map_or(now, |i| i.created_at),
        updated_at: now,
        latest: version,
        items: v.items.len(),
    };
    rows.push((format!("ds/{name}"), to_value(&info)));
    // Items and the version first, the latest pointer last: until the pointer is written the
    // version is not listed, and a retry writes the same version number again.
    rs.put(rows).await?;
    rs.flush().await?;
    tag_version(rs, &mut v).await?;
    Ok(v)
}

/// Tags every dataset version that has no storage tag yet: one whose commit was interrupted
/// before tagging, or one migrated from an earlier layout (whose storage versions no longer exist).
/// The current version of the dataset's table holds every version's items, since items are never
/// removed. Returns how many versions were tagged.
pub async fn tag_untagged_datasets(rs: &dyn RecordStore) -> Result<usize> {
    let mut n = 0;
    for info in datasets(rs).await? {
        for mut v in dataset_versions(rs, &info.name).await? {
            if v.storage_tag.is_none() {
                tag_version(rs, &mut v).await?;
                n += 1;
            }
        }
    }
    Ok(n)
}

async fn tag_version(rs: &dyn RecordStore, v: &mut DatasetVersion) -> Result<()> {
    // The dataset's items and versions share one storage table; the tag keeps only that table.
    let key = format!("dsv/{}/", v.name);
    let tag = dataset_tag(&v.name, v.version);
    // An existing tag (another process made it) is taken as it is.
    if let Some(sv) = rs.tag(&key, &tag).await? {
        v.storage_version = Some(sv);
        v.storage_tag = Some(tag);
        rs.put(vec![(
            format!("dsv/{}/{:08}", v.name, v.version),
            to_value(&*v),
        )])
        .await?;
        rs.flush().await?;
    }
    Ok(())
}

pub async fn datasets(rs: &dyn RecordStore) -> Result<Vec<DatasetInfo>> {
    rs.scan("ds/")
        .await?
        .into_iter()
        .map(|(_, v)| from_value(v))
        .collect()
}

/// Every version of a dataset, oldest first (without their item lists' contents).
pub async fn dataset_versions(rs: &dyn RecordStore, name: &str) -> Result<Vec<DatasetVersion>> {
    check_name("dataset", name)?;
    // A version newer than the latest pointer belongs to a commit that did not finish.
    let latest = match rs.get(&format!("ds/{name}")).await? {
        Some(v) => from_value::<DatasetInfo>(v)?.latest,
        None => return Ok(Vec::new()),
    };
    rs.scan(&format!("dsv/{name}/"))
        .await?
        .into_iter()
        .map(|(_, v)| from_value::<DatasetVersion>(v))
        .filter(|v| v.as_ref().map_or(true, |v| v.version <= latest))
        .collect()
}

/// One version, or the latest when `version` is `None`.
pub async fn dataset_version(
    rs: &dyn RecordStore,
    name: &str,
    version: Option<u32>,
) -> Result<DatasetVersion> {
    check_name("dataset", name)?;
    let version = match version {
        Some(v) => v,
        None => {
            let info: DatasetInfo = from_value(
                rs.get(&format!("ds/{name}"))
                    .await?
                    .ok_or_else(|| Error::NotFound(format!("dataset {name}")))?,
            )?;
            info.latest
        }
    };
    let latest = match rs.get(&format!("ds/{name}")).await? {
        Some(v) => from_value::<DatasetInfo>(v)?.latest,
        None => 0,
    };
    if version > latest {
        return Err(Error::NotFound(format!("dataset {name} version {version}")));
    }
    from_value(
        rs.get(&format!("dsv/{name}/{version:08}"))
            .await?
            .ok_or_else(|| Error::NotFound(format!("dataset {name} version {version}")))?,
    )
}

/// The items of a version, in order.
pub async fn dataset_items(rs: &dyn RecordStore, v: &DatasetVersion) -> Result<Vec<Item>> {
    let all: BTreeMap<String, Value> = rs
        .scan(&format!("dsi/{}/", v.name))
        .await?
        .into_iter()
        .collect();
    v.items
        .iter()
        .map(|r| {
            let row = all
                .get(&format!("dsi/{}/{}", v.name, r.hash))
                .cloned()
                .ok_or_else(|| Error::Storage(format!("item {} of {} is missing", r.id, v.name)))?;
            let mut item: Item = from_value(row)?;
            item.id = r.id.clone();
            Ok(item)
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Results, labels, features

pub async fn put_results(rs: &dyn RecordStore, results: Vec<EvalResult>) -> Result<()> {
    let rows = results
        .iter()
        .map(|r| (format!("res/{}/{}", r.run_id, r.item_id), to_value(r)))
        .collect();
    rs.put(rows).await
}

pub async fn results(rs: &dyn RecordStore, run_id: &str) -> Result<Vec<EvalResult>> {
    rs.scan(&format!("res/{run_id}/"))
        .await?
        .into_iter()
        .map(|(_, v)| from_value(v))
        .collect()
}

/// The results of several runs, keyed by run id, in one read (kv0 answers `run_id IN (...)` from
/// its run-id index; the whole results table is never loaded).
pub async fn results_of_runs(
    rs: &dyn RecordStore,
    run_ids: &[&str],
) -> Result<HashMap<String, Vec<EvalResult>>> {
    let mut out: HashMap<String, Vec<EvalResult>> = run_ids
        .iter()
        .map(|r| (r.to_string(), Vec::new()))
        .collect();
    for (key, v) in rs.scan_runs("res/", run_ids).await? {
        let run = key["res/".len()..].split('/').next().unwrap_or_default();
        if let Some(list) = out.get_mut(run) {
            list.push(from_value(v)?);
        }
    }
    Ok(out)
}

pub async fn put_label(rs: &dyn RecordStore, mut l: Label) -> Result<Label> {
    l.created_at = now_micros();
    rs.put(vec![(
        format!("lab/{}/{}/{:020}", l.run_id, l.item_id, l.created_at),
        to_value(&l),
    )])
    .await?;
    rs.flush().await?;
    Ok(l)
}

/// Labels of a run, oldest first per item.
pub async fn labels(rs: &dyn RecordStore, run_id: &str) -> Result<Vec<Label>> {
    rs.scan(&format!("lab/{run_id}/"))
        .await?
        .into_iter()
        .map(|(_, v)| from_value(v))
        .collect()
}

pub async fn put_features(rs: &dyn RecordStore, samples: Vec<FeatureSample>) -> Result<()> {
    let rows = samples
        .iter()
        .map(|s| (format!("feat/{}/{}", s.run_id, s.feature), to_value(s)))
        .collect();
    rs.put(rows).await
}

pub async fn features(rs: &dyn RecordStore, run_id: &str) -> Result<Vec<FeatureSample>> {
    rs.scan(&format!("feat/{run_id}/"))
        .await?
        .into_iter()
        .map(|(_, v)| from_value(v))
        .collect()
}

/// The feature samples of several runs, keyed by run id, in one read (see [`results_of_runs`]).
pub async fn features_of_runs(
    rs: &dyn RecordStore,
    run_ids: &[&str],
) -> Result<HashMap<String, Vec<FeatureSample>>> {
    let mut out: HashMap<String, Vec<FeatureSample>> = run_ids
        .iter()
        .map(|r| (r.to_string(), Vec::new()))
        .collect();
    for (key, v) in rs.scan_runs("feat/", run_ids).await? {
        let run = key["feat/".len()..].split('/').next().unwrap_or_default();
        if let Some(list) = out.get_mut(run) {
            list.push(from_value(v)?);
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Schedules, alerts, baselines

/// Stores a schedule run. Its output goes to a separate key (`schout/{schedule}/{id}`), so listing
/// runs never loads it and old output can be pruned sooner than the runs themselves.
pub async fn put_schedule_run(rs: &dyn RecordStore, r: &ScheduleRun) -> Result<()> {
    let mut summary = r.clone();
    summary.stdout.clear();
    summary.stderr.clear();
    let mut rows = vec![(format!("sch/{}/{}", r.schedule, r.id), to_value(&summary))];
    if !r.stdout.is_empty() || !r.stderr.is_empty() {
        rows.push((
            format!("schout/{}/{}", r.schedule, r.id),
            serde_json::json!({ "stdout": r.stdout, "stderr": r.stderr }),
        ));
    }
    rs.put(rows).await?;
    rs.flush().await
}

/// The captured output of one schedule run: (stdout, stderr), empty when none was kept.
pub async fn schedule_run_output(
    rs: &dyn RecordStore,
    schedule: &str,
    id: &str,
) -> Result<(String, String)> {
    Ok(match rs.get(&format!("schout/{schedule}/{id}")).await? {
        Some(v) => (
            v.get("stdout")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            v.get("stderr")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        ),
        None => (String::new(), String::new()),
    })
}

/// Ids of the runs of a schedule whose output is still kept.
pub async fn schedule_runs_with_output(
    rs: &dyn RecordStore,
    schedule: &str,
) -> Result<std::collections::HashSet<String>> {
    let prefix = format!("schout/{schedule}/");
    Ok(rs
        .keys(&prefix)
        .await?
        .into_iter()
        .filter_map(|k| k.strip_prefix(&prefix).map(str::to_string))
        .collect())
}

/// How long schedule runs and their output are kept.
#[derive(Clone, Copy, Debug)]
pub struct Retention {
    /// Runs kept per schedule, newest first.
    pub runs: usize,
    /// Runs older than this many days are removed.
    pub days: u32,
    /// Output is kept for this many of the newest runs only.
    pub output: usize,
}

/// Deletes schedule runs and output beyond the retention. Returns the number of keys removed.
pub async fn prune_schedule_runs(
    rs: &dyn RecordStore,
    schedule: &str,
    keep: Retention,
    now: i64,
) -> Result<usize> {
    let keys: Vec<String> = rs
        .scan(&format!("sch/{schedule}/"))
        .await?
        .into_iter()
        .map(|(k, _)| k)
        .collect();
    let cutoff = now - keep.days as i64 * 86_400_000_000;
    let mut delete = Vec::new();
    // Keys sort by start time; walk newest first.
    for (i, k) in keys.iter().rev().enumerate() {
        let id = k.rsplit('/').next().unwrap_or_default();
        let started: i64 = id
            .split('-')
            .next()
            .and_then(|t| t.parse().ok())
            .unwrap_or(now);
        if i >= keep.runs || started < cutoff {
            delete.push(k.clone());
            delete.push(format!("schout/{schedule}/{id}"));
        } else if i >= keep.output {
            delete.push(format!("schout/{schedule}/{id}"));
        }
    }
    let n = delete.len();
    if n > 0 {
        rs.delete(delete).await?;
        rs.flush().await?;
    }
    Ok(n)
}

/// Runs of a schedule, newest first, without their output (see [`schedule_run_output`]).
pub async fn schedule_runs(rs: &dyn RecordStore, schedule: &str) -> Result<Vec<ScheduleRun>> {
    let mut v: Vec<ScheduleRun> = rs
        .scan(&format!("sch/{schedule}/"))
        .await?
        .into_iter()
        .map(|(_, v)| from_value(v))
        .collect::<Result<_>>()?;
    v.reverse();
    Ok(v)
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ScheduleState {
    /// The last fire time that was handled (run, skipped or caught up).
    pub last_fire: Option<i64>,
}

pub async fn schedule_state(rs: &dyn RecordStore, schedule: &str) -> Result<ScheduleState> {
    Ok(match rs.get(&format!("schstate/{schedule}")).await? {
        Some(v) => from_value(v)?,
        None => ScheduleState::default(),
    })
}

pub async fn put_schedule_state(
    rs: &dyn RecordStore,
    schedule: &str,
    s: &ScheduleState,
) -> Result<()> {
    rs.put(vec![(format!("schstate/{schedule}"), to_value(s))])
        .await?;
    rs.flush().await
}

/// Stores `alert` unless one with the same dedupe key exists. Returns the stored alert when new.
///
/// The dedupe key is written first and holds the alert's id, so a failure between the two writes
/// can never raise (and send) the same alert again; the next check finds the key without its alert
/// and stores it then.
pub async fn raise_alert(rs: &dyn RecordStore, mut alert: Alert) -> Result<Option<Alert>> {
    let key = format!("alertkey/{}", alert.dedupe);
    match rs.get(&key).await? {
        Some(Value::String(id)) if rs.get(&format!("alert/{id}")).await?.is_some() => {
            return Ok(None);
        }
        // An earlier attempt wrote the key but not the alert: finish it under the same id.
        Some(Value::String(id)) => {
            alert.created_at = id
                .split('-')
                .next()
                .and_then(|t| t.parse().ok())
                .unwrap_or_else(now_micros);
            alert.id = id;
        }
        Some(_) => return Ok(None),
        None => {
            alert.created_at = now_micros();
            alert.id = format!(
                "{:020}-{}",
                alert.created_at,
                &hash_hex(alert.dedupe.as_bytes())[..6]
            );
            rs.put(vec![(key, Value::String(alert.id.clone()))]).await?;
            rs.flush().await?;
        }
    }
    rs.put(vec![(format!("alert/{}", alert.id), to_value(&alert))])
        .await?;
    rs.flush().await?;
    Ok(Some(alert))
}

pub async fn update_alert(rs: &dyn RecordStore, alert: &Alert) -> Result<()> {
    rs.put(vec![(format!("alert/{}", alert.id), to_value(alert))])
        .await?;
    rs.flush().await
}

/// Alerts, newest first.
pub async fn alerts(rs: &dyn RecordStore) -> Result<Vec<Alert>> {
    let mut v: Vec<Alert> = rs
        .scan("alert/")
        .await?
        .into_iter()
        .map(|(_, v)| from_value(v))
        .collect::<Result<_>>()?;
    v.reverse();
    Ok(v)
}

pub async fn baseline(rs: &dyn RecordStore, suite: &str) -> Result<Baseline> {
    Ok(match rs.get(&format!("baseline/{suite}")).await? {
        Some(v) => from_value(v)?,
        None => Baseline::default(),
    })
}

pub async fn put_baseline(rs: &dyn RecordStore, suite: &str, mut b: Baseline) -> Result<Baseline> {
    if b.mode == BaselineMode::Pinned && b.runs.is_empty() {
        return Err(Error::InvalidArgument(
            "a pinned baseline needs at least one run".into(),
        ));
    }
    b.window = b.window.clamp(1, 100);
    b.updated_at = now_micros();
    rs.put(vec![(format!("baseline/{suite}"), to_value(&b))])
        .await?;
    rs.flush().await?;
    Ok(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::records::MemoryRecords;
    use serde_json::json;

    fn item(id: &str, input: &str, expected: &str) -> ItemInput {
        ItemInput {
            id: Some(id.into()),
            input: json!(input),
            expected: json!(expected),
            metadata: json!({}),
        }
    }

    #[tokio::test]
    async fn dataset_versions_are_immutable_and_content_addressed() {
        let rs = MemoryRecords::new();
        let v1 = commit_dataset(
            &rs,
            "intents",
            vec![item("a", "hi", "greet"), item("b", "bye", "leave")],
            CommitMode::Replace,
            Some("first".into()),
            Some("Support intents".into()),
            Some("sdk".into()),
        )
        .await
        .unwrap();
        assert_eq!((v1.version, v1.changes.added), (1, 2));
        // The same items again: no new version.
        let same = commit_dataset(
            &rs,
            "intents",
            vec![item("a", "hi", "greet"), item("b", "bye", "leave")],
            CommitMode::Replace,
            None,
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(same.version, 1);
        // Upsert one changed and one new item.
        let v2 = commit_dataset(
            &rs,
            "intents",
            vec![item("b", "bye", "farewell"), item("c", "help", "support")],
            CommitMode::Upsert,
            None,
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(v2.version, 2);
        assert_eq!(
            v2.changes,
            Changes {
                added: 1,
                updated: 1,
                removed: 0
            }
        );
        assert_ne!(v1.digest, v2.digest);
        let items = dataset_items(
            &rs,
            &dataset_version(&rs, "intents", Some(1)).await.unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(
            items[1].expected,
            json!("leave"),
            "version 1 still has the old item"
        );
        let latest = dataset_version(&rs, "intents", None).await.unwrap();
        assert_eq!(latest.version, 2);
        let info = &datasets(&rs).await.unwrap()[0];
        assert_eq!(
            (info.latest, info.items, info.description.as_deref()),
            (2, 3, Some("Support intents"))
        );
        // Replace drops items.
        let v3 = commit_dataset(
            &rs,
            "intents",
            vec![item("a", "hi", "greet")],
            CommitMode::Replace,
            None,
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(v3.changes.removed, 2);
        assert!(commit_dataset(
            &rs,
            "bad/name",
            vec![],
            CommitMode::Replace,
            None,
            None,
            None
        )
        .await
        .is_err());
        assert!(commit_dataset(
            &rs,
            "dup",
            vec![item("x", "1", ""), item("x", "2", "")],
            CommitMode::Replace,
            None,
            None,
            None
        )
        .await
        .is_err());
    }

    #[tokio::test]
    async fn derived_ids_and_hashes_are_stable() {
        let a = new_item(ItemInput {
            id: None,
            input: json!({"b": 1, "a": 2}),
            ..Default::default()
        });
        let b = new_item(ItemInput {
            id: None,
            input: json!({"a": 2, "b": 1}),
            ..Default::default()
        });
        assert_eq!(a.id, b.id);
        assert_eq!(a.hash, b.hash);
        assert_eq!(a.id.len(), 12);
    }

    #[tokio::test]
    async fn alerts_dedupe_and_order() {
        let rs = MemoryRecords::new();
        let mk = |d: &str| Alert {
            id: String::new(),
            kind: AlertKind::ScheduleFailed,
            severity: Severity::Critical,
            title: "t".into(),
            detail: "d".into(),
            suite: None,
            run_id: None,
            schedule: Some("nightly".into()),
            created_at: 0,
            acknowledged_at: None,
            dedupe: d.into(),
            delivered: None,
        };
        assert!(raise_alert(&rs, mk("x")).await.unwrap().is_some());
        assert!(raise_alert(&rs, mk("x")).await.unwrap().is_none());
        assert!(raise_alert(&rs, mk("y")).await.unwrap().is_some());
        let all = alerts(&rs).await.unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].dedupe, "y");
    }

    /// Records that fail the next write whose keys start with `fail_prefix`, or the next tag;
    /// versioned like the kv0 store.
    #[derive(Default)]
    struct Flaky {
        inner: MemoryRecords,
        fail_prefix: parking_lot::Mutex<Option<String>>,
        fail_tag: std::sync::atomic::AtomicBool,
        version: std::sync::atomic::AtomicU64,
        tags: parking_lot::Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl RecordStore for Flaky {
        async fn put(&self, rows: Vec<(String, Value)>) -> Result<()> {
            let fail = self.fail_prefix.lock().clone();
            if let Some(p) = fail {
                if rows.iter().any(|(k, _)| k.starts_with(&p)) {
                    *self.fail_prefix.lock() = None;
                    return Err(Error::Storage("injected failure".into()));
                }
            }
            self.version
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.inner.put(rows).await
        }
        async fn get(&self, key: &str) -> Result<Option<Value>> {
            self.inner.get(key).await
        }
        async fn scan(&self, prefix: &str) -> Result<Vec<(String, Value)>> {
            self.inner.scan(prefix).await
        }
        async fn delete(&self, keys: Vec<String>) -> Result<()> {
            self.inner.delete(keys).await
        }
        async fn flush(&self) -> Result<()> {
            Ok(())
        }
        async fn refresh(&self) -> Result<()> {
            Ok(())
        }
        async fn storage_version(&self, _key: &str) -> Result<Option<u64>> {
            Ok(Some(
                self.version.load(std::sync::atomic::Ordering::Relaxed),
            ))
        }
        async fn tag(&self, _key: &str, name: &str) -> Result<Option<u64>> {
            if self
                .fail_tag
                .swap(false, std::sync::atomic::Ordering::Relaxed)
            {
                return Err(Error::Storage("injected tag failure".into()));
            }
            self.tags.lock().push(name.to_string());
            Ok(Some(
                self.version.load(std::sync::atomic::Ordering::Relaxed),
            ))
        }
    }

    fn alert(d: &str) -> Alert {
        Alert {
            id: String::new(),
            kind: AlertKind::ScheduleFailed,
            severity: Severity::Critical,
            title: "t".into(),
            detail: "d".into(),
            suite: None,
            run_id: None,
            schedule: Some("nightly".into()),
            created_at: 0,
            acknowledged_at: None,
            dedupe: d.into(),
            delivered: None,
        }
    }

    #[tokio::test]
    async fn an_alert_whose_second_write_fails_is_raised_once() {
        let rs = Flaky::default();
        *rs.fail_prefix.lock() = Some("alert/".into());
        assert!(raise_alert(&rs, alert("x")).await.is_err());
        // The key was written; the next check stores the alert under the same id, once.
        let healed = raise_alert(&rs, alert("x"))
            .await
            .unwrap()
            .expect("stored now");
        assert!(raise_alert(&rs, alert("x")).await.unwrap().is_none());
        assert!(raise_alert(&rs, alert("x")).await.unwrap().is_none());
        let all = alerts(&rs).await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].id, healed.id);
    }

    #[tokio::test]
    async fn dataset_commits_heal_after_failures() {
        let rs = Flaky::default();
        let items = || vec![item("a", "hi", "greet"), item("b", "bye", "leave")];
        let v1 = commit_dataset(&rs, "qa", items(), CommitMode::Replace, None, None, None)
            .await
            .unwrap();
        assert!(v1.storage_tag.is_some());

        // The second commit's rows (items, version and pointer) fail: nothing is listed.
        *rs.fail_prefix.lock() = Some("dsv/".into());
        let more = || vec![item("a", "hi", "greet"), item("c", "thanks", "close")];
        assert!(
            commit_dataset(&rs, "qa", more(), CommitMode::Replace, None, None, None)
                .await
                .is_err()
        );
        assert_eq!(dataset_versions(&rs, "qa").await.unwrap().len(), 1);

        // The retry commits version 2, but tagging fails: the version is listed without a tag.
        rs.fail_tag
            .store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(
            commit_dataset(&rs, "qa", more(), CommitMode::Replace, None, None, None)
                .await
                .is_err()
        );
        let v2 = dataset_version(&rs, "qa", None).await.unwrap();
        assert_eq!((v2.version, v2.storage_tag.as_deref()), (2, None));

        // Committing the same content again completes the tag.
        let v2 = commit_dataset(&rs, "qa", more(), CommitMode::Replace, None, None, None)
            .await
            .unwrap();
        assert_eq!(v2.version, 2);
        assert_eq!(
            v2.storage_tag.as_deref(),
            Some(dataset_tag("qa", 2).as_str())
        );
        assert_eq!(
            dataset_version(&rs, "qa", None).await.unwrap().storage_tag,
            v2.storage_tag
        );
        assert_eq!(rs.tags.lock().len(), 2);
    }

    #[tokio::test]
    async fn versions_beyond_the_latest_pointer_are_hidden() {
        let rs = MemoryRecords::new();
        commit_dataset(
            &rs,
            "qa",
            vec![item("a", "hi", "greet")],
            CommitMode::Replace,
            None,
            None,
            None,
        )
        .await
        .unwrap();
        // A version written by a commit that never wrote its pointer.
        let mut orphan = dataset_version(&rs, "qa", Some(1)).await.unwrap();
        orphan.version = 2;
        rs.put(vec![("dsv/qa/00000002".into(), to_value(&orphan))])
            .await
            .unwrap();
        assert_eq!(dataset_versions(&rs, "qa").await.unwrap().len(), 1);
        assert!(matches!(
            dataset_version(&rs, "qa", Some(2)).await,
            Err(Error::NotFound(_))
        ));
    }

    #[tokio::test]
    async fn schedule_output_is_separate_and_pruned() {
        let rs = MemoryRecords::new();
        let day = 86_400_000_000i64;
        let now = 400 * day;
        for i in 0..10i64 {
            let started = now - (9 - i) * day * 20;
            let r = ScheduleRun {
                id: format!("{started:020}-1"),
                schedule: "nightly".into(),
                scheduled_for: started,
                started_at: started,
                ended_at: Some(started + 1),
                attempt: 1,
                status: ScheduleRunStatus::Succeeded,
                exit_code: Some(0),
                command: vec!["true".into()],
                stdout: format!("run {i}"),
                stderr: String::new(),
                reason: None,
            };
            put_schedule_run(&rs, &r).await.unwrap();
        }
        let runs = schedule_runs(&rs, "nightly").await.unwrap();
        assert_eq!(runs.len(), 10);
        assert!(runs[0].stdout.is_empty(), "listing does not load output");
        assert_eq!(
            schedule_run_output(&rs, "nightly", &runs[0].id)
                .await
                .unwrap()
                .0,
            "run 9"
        );
        // Keep 6 runs, none older than 90 days, output for the newest 2.
        prune_schedule_runs(
            &rs,
            "nightly",
            Retention {
                runs: 6,
                days: 90,
                output: 2,
            },
            now,
        )
        .await
        .unwrap();
        let runs = schedule_runs(&rs, "nightly").await.unwrap();
        assert_eq!(runs.len(), 5, "the 6th newest is 100 days old");
        assert_eq!(
            schedule_run_output(&rs, "nightly", &runs[1].id)
                .await
                .unwrap()
                .0,
            "run 8"
        );
        assert_eq!(
            schedule_run_output(&rs, "nightly", &runs[2].id)
                .await
                .unwrap()
                .0,
            ""
        );
    }
}
