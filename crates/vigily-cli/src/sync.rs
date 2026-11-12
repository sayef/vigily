//! `vigily sync`: sends what the Python SDK left in its offline spool.
//!
//! The SDK keeps batches it could not send in `<state dir>/spool/<writer>/`, one directory per
//! writing process (python/vigily/_spool.py writes them):
//!
//! | File                    | What it holds |
//! |-------------------------|---------------|
//! | `meta.json`             | `{"version": 1, "writer", "pid", "url", "repo"}`: where the batches go (no token) |
//! | `lock`                  | Locked by the writing process while it runs |
//! | `<n>-b.ndjson.gz`       | Ingest batches, one [`IngestBatch`] per line |
//! | `<n>-s.ndjson.gz`       | Spans, one `{"spans": [...]}` per line |
//!
//! `<n>` counts up from 1 in 12 digits; files are sent in that order and each is removed once the
//! engine has taken all of it. Batches keep their writer and sequence number, so sending one twice
//! (here and by a process that retries it) stores it once.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result};
use serde::Deserialize;
use vigily_api::client::{Client, Error};
use vigily_api::ingest::{IngestBatch, Op};
use vigily_api::local;
use vigily_api::traces::IngestBody;

/// What `meta.json` says about a spool directory.
#[derive(Clone, Debug, Deserialize)]
pub struct SpoolMeta {
    #[serde(default)]
    pub writer: String,
    #[serde(default)]
    pub pid: Option<u32>,
    /// The engine address the SDK was given, or null.
    #[serde(default)]
    pub url: Option<String>,
    /// The repository (canonical location) whose local engine the SDK used, or null.
    #[serde(default)]
    pub repo: Option<String>,
}

/// Which spooled batches this engine takes: those sent to its address, or to its repository.
#[derive(Clone, Debug)]
pub enum Target {
    Url(String),
    Repo(String),
}

impl Target {
    fn matches(&self, m: &SpoolMeta) -> bool {
        match self {
            Target::Url(u) => m.url.as_deref().map(|x| x.trim_end_matches('/')) == Some(u.as_str()),
            Target::Repo(r) => m.url.is_none() && m.repo.as_deref() == Some(r.as_str()),
        }
    }
}

/// `<state dir>/spool`.
pub fn spool_root() -> Option<PathBuf> {
    local::state_dir().map(|d| d.join("spool"))
}

/// A spooled file: its number, kind (`b` or `s`) and path.
fn files(dir: &Path) -> Vec<(u64, char, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<(u64, char, PathBuf)> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let rest = name.strip_suffix(".ndjson.gz")?;
            let (n, kind) = rest.split_once('-')?;
            let kind = match kind {
                "b" => 'b',
                "s" => 's',
                _ => return None,
            };
            if n.len() != 12 || !n.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            Some((n.parse().ok()?, kind, e.path()))
        })
        .collect();
    out.sort();
    out
}

fn read_lines(path: &Path) -> Result<Vec<String>> {
    let f =
        std::fs::File::open(path).with_context(|| format!("could not open {}", path.display()))?;
    let mut text = String::new();
    flate2::read::GzDecoder::new(f)
        .read_to_string(&mut text)
        .with_context(|| format!("could not read {}", path.display()))?;
    Ok(text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect())
}

/// The run an operation writes to.
fn op_run(op: &Op) -> Option<&str> {
    match op {
        Op::Create(c) => Some(&c.id),
        Op::Params { run_id, .. }
        | Op::Update { run_id, .. }
        | Op::Heartbeat { run_id }
        | Op::Finish { run_id, .. } => Some(run_id),
        Op::Points(p) => Some(&p.run_id),
        Op::Results { results } => results.first().map(|r| r.run_id.as_str()),
        Op::Features { samples } => samples.first().map(|s| s.run_id.as_str()),
    }
}

/// `operation N (kind): ...` names the failed operation, counting from 1.
fn op_index(message: &str) -> Option<usize> {
    let rest = message.strip_prefix("operation ")?;
    let n: usize = rest.split(' ').next()?.parse().ok()?;
    n.checked_sub(1)
}

/// `run <id> was deleted ...`.
fn deleted_run(message: &str) -> Option<String> {
    let (_, rest) = message.split_once("run ")?;
    let (id, tail) = rest.split_once(' ')?;
    tail.starts_with("was deleted").then(|| id.to_string())
}

/// Whether the spool holds files for `target`.
pub fn has_work(root: &Path, target: &Target) -> bool {
    let Ok(entries) = std::fs::read_dir(root) else {
        return false;
    };
    entries.filter_map(|e| e.ok()).any(|e| {
        std::fs::read_to_string(e.path().join("meta.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<SpoolMeta>(&t).ok())
            .is_some_and(|m| target.matches(&m) && !files(&e.path()).is_empty())
    })
}

/// What one directory sent.
#[derive(Default)]
struct Sent {
    batches: usize,
    points: usize,
    spans: usize,
    dropped: Vec<String>,
}

/// Sends one batch; operations the engine cannot store (a deleted run, a refused operation) are
/// left out and the rest sent again.
async fn send_batch(c: &Client, mut batch: IngestBatch, sent: &mut Sent) -> Result<(), Error> {
    let key = format!("{}:{}", batch.writer, batch.seq);
    loop {
        match c.ingest(&batch, Some(&key)).await {
            Ok(r) => {
                sent.batches += 1;
                sent.points += r.points;
                return Ok(());
            }
            Err(Error::Api(a))
                if (400..500).contains(&a.status) && ![401, 403, 408, 429].contains(&a.status) =>
            {
                let n = op_index(&a.message).filter(|n| *n < batch.ops.len());
                if let Some(run) = deleted_run(&a.message).filter(|_| a.status == 410) {
                    let before = batch.ops.len();
                    batch.ops.retain(|op| op_run(op) != Some(run.as_str()));
                    if batch.ops.len() < before {
                        sent.dropped.push(format!(
                            "run {run} was deleted; its spooled writes were not stored"
                        ));
                        continue;
                    }
                }
                let Some(n) = n else {
                    return Err(Error::Api(a));
                };
                sent.dropped
                    .push(format!("the engine refused an operation ({})", a.message));
                batch.ops.remove(n);
            }
            Err(e) => return Err(e),
        }
    }
}

/// Sends a spool directory's files in order, removing each once sent. Stops at the first error
/// the engine might not repeat (unreachable, busy, a refused token).
async fn send_dir(c: &Client, dir: &Path, sent: &mut Sent) -> Result<()> {
    for (_, kind, path) in files(dir) {
        for line in read_lines(&path)? {
            if kind == 'b' {
                let batch: IngestBatch = serde_json::from_str(&line).with_context(|| {
                    format!("{} holds a line that is not a batch", path.display())
                })?;
                send_batch(c, batch, sent)
                    .await
                    .map_err(|e| crate::explain(e, c))?;
            } else {
                let body: IngestBody = serde_json::from_str(&line).with_context(|| {
                    format!("{} holds a line that is not spans", path.display())
                })?;
                let n = body.spans.len();
                match c.ingest_spans(&body).await {
                    Ok(_) => sent.spans += n,
                    Err(Error::Api(a))
                        if (400..500).contains(&a.status)
                            && ![401, 403, 408, 429].contains(&a.status) =>
                    {
                        sent.dropped
                            .push(format!("the engine refused {n} spans (HTTP {})", a.status));
                    }
                    Err(e) => return Err(crate::explain(e, c)),
                }
            }
        }
        std::fs::remove_file(&path)
            .with_context(|| format!("could not remove {}", path.display()))?;
    }
    Ok(())
}

fn remove_dir(dir: &Path) {
    for f in ["meta.json", "lock"] {
        let _ = std::fs::remove_file(dir.join(f));
    }
    let _ = std::fs::remove_dir(dir);
}

/// Sends every spool directory meant for `target` to `c`, oldest writer first, and names the
/// directories meant for other engines.
pub async fn sync(
    c: &Client,
    target: &Target,
    root: &Path,
    dry_run: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<ExitCode> {
    let mut dirs: Vec<PathBuf> = match std::fs::read_dir(root) {
        Ok(entries) => entries.filter_map(|e| e.ok()).map(|e| e.path()).collect(),
        Err(_) => Vec::new(),
    };
    dirs.sort();
    let mut total = Sent::default();
    let (mut done, mut busy) = (0usize, 0usize);
    let mut others: Vec<String> = Vec::new();
    let mut failed = false;
    for dir in dirs {
        let Ok(text) = std::fs::read_to_string(dir.join("meta.json")) else {
            continue;
        };
        let Ok(meta) = serde_json::from_str::<SpoolMeta>(&text) else {
            writeln!(err, "skipping {}: meta.json is not readable", dir.display())?;
            continue;
        };
        let pending = files(&dir);
        if pending.is_empty() {
            continue;
        }
        if !target.matches(&meta) {
            others.push(
                meta.url
                    .clone()
                    .or_else(|| meta.repo.clone())
                    .unwrap_or_else(|| "an unknown engine".into()),
            );
            continue;
        }
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join("lock"))
            .with_context(|| format!("could not open {}", dir.join("lock").display()))?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                busy += 1;
                writeln!(
                    err,
                    "skipping {}: process {} is still running and sends it itself",
                    dir.display(),
                    meta.pid.map_or_else(|| "?".to_string(), |p| p.to_string())
                )?;
                continue;
            }
            Err(std::fs::TryLockError::Error(e)) => {
                return Err(e).with_context(|| format!("could not lock {}", dir.display()))
            }
        }
        if dry_run {
            writeln!(
                out,
                "{}: {} files from writer {}",
                dir.display(),
                pending.len(),
                meta.writer
            )?;
            continue;
        }
        let mut sent = Sent::default();
        let r = send_dir(c, &dir, &mut sent).await;
        total.batches += sent.batches;
        total.points += sent.points;
        total.spans += sent.spans;
        total.dropped.append(&mut sent.dropped);
        match r {
            Ok(()) => {
                done += 1;
                drop(lock);
                remove_dir(&dir);
            }
            Err(e) => {
                failed = true;
                writeln!(
                    err,
                    "error: {}: {e:#}; the rest stays in the spool",
                    dir.display()
                )?;
                break;
            }
        }
    }
    for d in &total.dropped {
        writeln!(err, "{d}")?;
    }
    if !dry_run {
        writeln!(
            out,
            "Sent {} batches ({} points) and {} spans from {} spool {} to {}.",
            total.batches,
            total.points,
            total.spans,
            done,
            if done == 1 {
                "directory"
            } else {
                "directories"
            },
            c.base()
        )?;
    }
    if busy > 0 {
        writeln!(out, "{busy} spool directories belong to running processes.")?;
    }
    if !others.is_empty() {
        others.sort();
        others.dedup();
        writeln!(
            out,
            "The spool also holds batches for other engines; run vigily sync with their --url or --repo: {}",
            others.join(", ")
        )?;
    }
    Ok(if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_name_the_operation_and_the_run() {
        assert_eq!(op_index("operation 3 (points): run x was deleted"), Some(2));
        assert_eq!(op_index("something else"), None);
        assert_eq!(
            deleted_run("operation 3 (points): run 0190ab was deleted or never created; nothing"),
            Some("0190ab".into())
        );
        assert_eq!(deleted_run("no run here"), None);
    }

    #[test]
    fn only_spool_files_are_listed_in_order() {
        let dir = tempfile::tempdir().unwrap();
        for f in [
            "000000000002-s.ndjson.gz",
            "000000000001-b.ndjson.gz",
            "000000000003-b.ndjson.gz.tmp-1-2",
            "meta.json",
            "1-b.ndjson.gz",
        ] {
            std::fs::write(dir.path().join(f), b"").unwrap();
        }
        let got: Vec<(u64, char)> = files(dir.path())
            .into_iter()
            .map(|(n, k, _)| (n, k))
            .collect();
        assert_eq!(got, vec![(1, 'b'), (2, 's')]);
    }

    #[test]
    fn targets_match_by_address_or_repository() {
        let m = |url: Option<&str>, repo: Option<&str>| SpoolMeta {
            writer: "w".into(),
            pid: None,
            url: url.map(str::to_string),
            repo: repo.map(str::to_string),
        };
        let url = Target::Url("http://h:1".into());
        assert!(url.matches(&m(Some("http://h:1/"), None)));
        assert!(!url.matches(&m(None, Some("/r"))));
        let repo = Target::Repo("/r".into());
        assert!(repo.matches(&m(None, Some("/r"))));
        assert!(!repo.matches(&m(Some("http://h:1"), Some("/r"))));
    }
}
