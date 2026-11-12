//! Writing runs in batches: `POST ingest`.
//!
//! A client sends an ordered log of operations in batches. Each batch names its writer (a random
//! id the client picks once per process) and a sequence number counting up from 1, so the engine
//! recognises a batch it already applied and answers the first report again instead of applying it
//! twice. Clients assign every point's step themselves, so even a batch the engine no longer
//! remembers (after a restart) stores nothing twice: a point replaces the one at the same step of
//! the same series.
//!
//! The body is one [`IngestBatch`] as JSON (`application/json`), or several, one per line
//! (`application/x-ndjson`), applied in order. Either may be gzip-compressed
//! (`Content-Encoding: gzip`).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::evals::{EvalResult, FeatureSample};
use crate::model::{Micros, RunPatch, RunStatus};
use crate::runs::{CreateRun, SeriesPoints};

/// One batch of operations from one writer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct IngestBatch {
    /// The sending process: an id the client picks once, such as a random UUID.
    pub writer: String,
    /// This batch's number for its writer: 1, 2, 3, ... A writer sends its batches in order; a
    /// batch at or below the highest number applied is a replay.
    pub seq: u64,
    pub ops: Vec<Op>,
}

/// One operation of a batch. Operations are applied in order: create a run before writing to it,
/// and send its results before finishing it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    /// Creates a run, or does nothing when a run with this id exists.
    Create(CreateRun),
    /// Merges a nested object into the run's parameters.
    Params { run_id: String, params: Value },
    /// Points of one series in columns: `steps` and `values` of the same length, and optionally
    /// `epochs` and `timestamps` of that length too.
    Points(SeriesPoints),
    /// Changes the run's metadata.
    Update { run_id: String, patch: RunPatch },
    /// The writing process is alive. Stored as sent; any other operation on a run counts as well,
    /// of which the engine stores at most one heartbeat per run every two minutes.
    Heartbeat { run_id: String },
    /// Ends the run, after everything before it in the batch is in storage.
    Finish {
        run_id: String,
        status: RunStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ended_at: Option<Micros>,
    },
    /// Per-item eval results (each names its run).
    Results { results: Vec<EvalResult> },
    /// Input feature samples (each names its run).
    Features { samples: Vec<FeatureSample> },
}

/// What a batch wrote.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct IngestReport {
    pub writer: String,
    pub seq: u64,
    /// Operations applied.
    pub ops: usize,
    /// Runs this batch created (a create of an existing run does not count).
    pub runs_created: usize,
    pub points: usize,
    pub results: usize,
    pub features: usize,
    /// The engine had applied this batch before (the same `Idempotency-Key`, or a `seq` at or
    /// below the writer's highest): nothing was written again, and the counts are the first
    /// application's when the engine still has them.
    pub replayed: bool,
}

/// The answer to an NDJSON ingest: one report per batch, in order.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct IngestReports {
    pub batches: Vec<IngestReport>,
}
