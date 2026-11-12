//! The engine's write state: open run writers, the batches each client writer has applied, and
//! the memory budget of ingest requests in flight.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::Value;
use tokio::sync::Semaphore;
use vigily_api::ingest::IngestReport;
use vigily_core::RunWriter;

/// The largest request body of the write routes after decompression: 32 MiB.
pub const MAX_BODY: usize = 32 * 1024 * 1024;

/// The bytes of ingest bodies the engine holds at once: 256 MiB. Above it, ingest answers 503 with
/// `Retry-After`.
pub const INGEST_BUDGET: usize = 256 * 1024 * 1024;

/// A run writer nobody wrote to for this long is closed (it reopens on the next write).
pub const WRITER_IDLE: Duration = Duration::from_secs(15 * 60);

/// At most this many run writers stay open; the least recently used is closed first.
pub const MAX_WRITERS: usize = 4096;

/// At most one heartbeat row per run in this interval, however many operations arrive.
pub const HEARTBEAT_EVERY: Duration = Duration::from_secs(120);

/// How long an ingest `Idempotency-Key` is remembered.
pub const KEY_TTL: Duration = Duration::from_secs(15 * 60);

/// Reports kept per client writer, to answer replays of recent batches.
const REPORTS_PER_WRITER: usize = 64;

/// The limits of the write routes. [`Default`] is [`MAX_BODY`] and [`INGEST_BUDGET`].
#[derive(Clone, Copy, Debug)]
pub struct WriteLimits {
    pub max_body: usize,
    pub ingest_budget: usize,
}

impl Default for WriteLimits {
    fn default() -> Self {
        Self {
            max_body: MAX_BODY,
            ingest_budget: INGEST_BUDGET,
        }
    }
}

/// The write state of an engine.
pub struct Writes {
    pub limits: WriteLimits,
    pub writers: WriterRegistry,
    pub idempotency: IdempotencyCache,
    /// The ingest budget in KiB permits.
    pub budget: Arc<Semaphore>,
    /// Serialises run creation, so two creates of one id make one run.
    pub(crate) create_lock: tokio::sync::Mutex<()>,
    /// Serialises dataset commits (and their idempotency records).
    pub(crate) commit_lock: tokio::sync::Mutex<()>,
}

impl Writes {
    pub fn new(limits: WriteLimits) -> Self {
        Self {
            limits,
            writers: WriterRegistry::default(),
            idempotency: IdempotencyCache::default(),
            budget: Arc::new(Semaphore::new(kib(limits.ingest_budget))),
            create_lock: tokio::sync::Mutex::new(()),
            commit_lock: tokio::sync::Mutex::new(()),
        }
    }

    /// The permits a body of `bytes` takes, at most the whole budget.
    pub(crate) fn permits(&self, bytes: usize) -> u32 {
        kib(bytes.min(self.limits.ingest_budget)).max(1) as u32
    }
}

fn kib(bytes: usize) -> usize {
    bytes.div_ceil(1024).min(u32::MAX as usize)
}

struct Slot {
    writer: Arc<dyn RunWriter>,
    last_used: Instant,
    last_heartbeat: Instant,
}

/// Open run writers by run id, closed after [`WRITER_IDLE`] without writes, when the run is
/// finished or deleted, or when more than [`MAX_WRITERS`] are open (least recently used first).
pub struct WriterRegistry {
    slots: Mutex<HashMap<String, Slot>>,
    last_sweep: Mutex<Instant>,
}

impl Default for WriterRegistry {
    fn default() -> Self {
        Self {
            slots: Mutex::new(HashMap::new()),
            last_sweep: Mutex::new(Instant::now()),
        }
    }
}

impl WriterRegistry {
    /// The open writer of a run, marked as used.
    pub fn get(&self, run_id: &str) -> Option<Arc<dyn RunWriter>> {
        let mut slots = self.slots.lock();
        let slot = slots.get_mut(run_id)?;
        slot.last_used = Instant::now();
        Some(slot.writer.clone())
    }

    /// Keeps a writer open. Creating a run (`created`) stores a row and counts as its heartbeat;
    /// after opening an existing run the first write also stores a heartbeat.
    pub fn insert(&self, run_id: &str, writer: Arc<dyn RunWriter>, created: bool) {
        let now = Instant::now();
        self.sweep(now);
        let mut slots = self.slots.lock();
        slots.insert(
            run_id.to_string(),
            Slot {
                writer,
                last_used: now,
                last_heartbeat: if created {
                    now
                } else {
                    now.checked_sub(HEARTBEAT_EVERY).unwrap_or(now)
                },
            },
        );
        while slots.len() > MAX_WRITERS {
            let Some(oldest) = slots
                .iter()
                .min_by_key(|(_, s)| s.last_used)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            slots.remove(&oldest);
        }
    }

    /// Closes a run's writer (finished or deleted).
    pub fn remove(&self, run_id: &str) {
        self.slots.lock().remove(run_id);
    }

    /// The number of open writers.
    pub fn len(&self) -> usize {
        self.slots.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether a heartbeat row is due for this run, and if so marks it written: true at most once
    /// per [`HEARTBEAT_EVERY`].
    pub fn heartbeat_due(&self, run_id: &str) -> bool {
        let now = Instant::now();
        let mut slots = self.slots.lock();
        match slots.get_mut(run_id) {
            Some(s) if now.duration_since(s.last_heartbeat) >= HEARTBEAT_EVERY => {
                s.last_heartbeat = now;
                true
            }
            _ => false,
        }
    }

    /// Closes writers idle for [`WRITER_IDLE`], at most once a minute.
    fn sweep(&self, now: Instant) {
        {
            let mut last = self.last_sweep.lock();
            if now.duration_since(*last) < Duration::from_secs(60) {
                return;
            }
            *last = now;
        }
        self.slots
            .lock()
            .retain(|_, s| now.duration_since(s.last_used) < WRITER_IDLE);
    }
}

/// The batches a client writer applied: the highest sequence number and the latest reports.
#[derive(Default)]
pub struct WriterSeq {
    pub high_water: Option<u64>,
    reports: VecDeque<IngestReport>,
}

impl WriterSeq {
    /// The report to answer for a batch already applied, or `None` for a new batch.
    pub fn replay(&self, writer: &str, seq: u64) -> Option<IngestReport> {
        if self.high_water.is_none_or(|h| seq > h) {
            return None;
        }
        let mut r = self
            .reports
            .iter()
            .find(|r| r.seq == seq)
            .cloned()
            .unwrap_or_else(|| IngestReport {
                writer: writer.to_string(),
                seq,
                ..Default::default()
            });
        r.replayed = true;
        Some(r)
    }

    /// Records an applied batch.
    pub fn applied(&mut self, report: &IngestReport) {
        self.high_water = Some(self.high_water.unwrap_or(0).max(report.seq));
        self.reports.push_back(report.clone());
        while self.reports.len() > REPORTS_PER_WRITER {
            self.reports.pop_front();
        }
    }
}

/// A client writer's state, locked while one of its batches is applied.
pub type SharedSeq = Arc<tokio::sync::Mutex<WriterSeq>>;

/// What the engine remembers to answer replayed ingest requests: per client writer the batches it
/// applied, and recent `Idempotency-Key`s with their answers. Held in memory only: after a restart
/// a replayed batch is applied again, which stores nothing twice because clients assign steps.
#[derive(Default)]
pub struct IdempotencyCache {
    writers: Mutex<HashMap<String, (Instant, SharedSeq)>>,
    keys: Mutex<HashMap<String, (Instant, Value)>>,
}

/// Client writers remembered at most; the least recently seen are forgotten first.
const MAX_CLIENT_WRITERS: usize = 10_000;

/// Idempotency keys remembered at most.
const MAX_KEYS: usize = 10_000;

impl IdempotencyCache {
    /// The state of a client writer; lock it while applying its batch.
    pub fn writer(&self, writer: &str) -> SharedSeq {
        let now = Instant::now();
        let mut w = self.writers.lock();
        if w.len() >= MAX_CLIENT_WRITERS && !w.contains_key(writer) {
            if let Some(oldest) = w.iter().min_by_key(|(_, v)| v.0).map(|(k, _)| k.clone()) {
                w.remove(&oldest);
            }
        }
        let e = w
            .entry(writer.to_string())
            .or_insert_with(|| (now, Arc::default()));
        e.0 = now;
        e.1.clone()
    }

    /// The answer of an earlier request with this `Idempotency-Key`.
    pub fn answer(&self, key: &str) -> Option<Value> {
        let mut keys = self.keys.lock();
        match keys.get(key) {
            Some((at, v)) if at.elapsed() < KEY_TTL => Some(v.clone()),
            Some(_) => {
                keys.remove(key);
                None
            }
            None => None,
        }
    }

    /// Remembers the answer of a request with an `Idempotency-Key`.
    pub fn remember(&self, key: &str, answer: Value) {
        let now = Instant::now();
        let mut keys = self.keys.lock();
        if keys.len() >= MAX_KEYS {
            keys.retain(|_, (at, _)| now.duration_since(*at) < KEY_TTL);
            if keys.len() >= MAX_KEYS {
                if let Some(oldest) = keys.iter().min_by_key(|(_, v)| v.0).map(|(k, _)| k.clone()) {
                    keys.remove(&oldest);
                }
            }
        }
        keys.insert(key.to_string(), (now, answer));
    }
}
