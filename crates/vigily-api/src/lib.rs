//! # vigily-api
//!
//! The requests and answers of vigily's HTTP API, shared by the engine (`vigily-server`), the web
//! app, the command line and the SDK. Only data types live here: serde structs with the small
//! amount of pure logic that belongs to them (a run's duration, a series' running statistics, the
//! tracing policy of a project). Storage and computation stay in `vigily-core`, which re-exports
//! these types, so engine code uses them under their old paths.
//!
//! - [`model`]: runs, parameters, metric series, summaries and metric keys (`loss[val]`).
//! - [`catalog`]: series with their statistics; experiments, metric and parameter keys.
//! - [`runs`]: rows of the runs table, a run's detail, the repository summary, run deletion.
//! - [`chart`]: the x axis, aligned series, group bands, and the chart request and answer.
//! - [`evals`]: datasets, results, labels, feature samples, schedules, alerts, baselines and the
//!   answers of their routes.
//! - [`drift`]: suites and the behaviour- and data-drift verdicts.
//! - [`traces`]: spans, trace summaries, the payload policy, and the traces list and detail.
//!
//! The wire format is pinned by golden JSON files in `tests/fixtures`: every answer type reads
//! its fixture and writes it back unchanged, keys in the same order.

pub mod catalog;
pub mod chart;
pub mod drift;
pub mod evals;
pub mod model;
pub mod runs;
pub mod traces;

use serde::{Deserialize, Serialize};

/// The version of the API this crate describes: routes live under `/api/v1`.
pub const API_VERSION: &str = "v1";

/// The error codes of `/api/v1` (`error.code`). A client branches on the code, never on the
/// message; codes are only ever added.
pub mod code {
    /// No such run, dataset, trace, schedule, suite or route.
    pub const NOT_FOUND: &str = "not_found";
    /// A malformed request: bad JSON, an invalid filter or parameter.
    pub const INVALID_ARGUMENT: &str = "invalid_argument";
    /// The request contradicts the current state (deleting a running run, for example).
    pub const CONFLICT: &str = "conflict";
    /// The run was deleted while a client was still writing to it.
    pub const RUN_DELETED: &str = "run_deleted";
    /// No or an unknown token.
    pub const UNAUTHORIZED: &str = "unauthorized";
    /// The token or the request's origin may not do this.
    pub const FORBIDDEN: &str = "forbidden";
    /// The body is larger than the engine accepts.
    pub const TOO_LARGE: &str = "too_large";
    /// The engine is busy; retry after the `Retry-After` header's seconds.
    pub const OVERLOADED: &str = "overloaded";
    /// Something failed inside the engine (storage, for example).
    pub const INTERNAL: &str = "internal";
    /// `POST admin/purge` (and an erasing `POST traces/delete`) without `force`: a storage tag
    /// keeps an older version that still holds deleted data, so nothing was purged.
    pub const TAG_BLOCKS_PURGE: &str = "tag_blocks_purge";
}

/// Every failed `/api/v1` request: `{"error": {"code", "message", "position"?}}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

/// Why a request failed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ErrorDetail {
    /// One of [`code`], or a more specific code a route documents.
    pub code: String,
    /// A sentence that names the problem and the way out.
    pub message: String,
    /// For a run filter that does not parse: the byte offset of the problem.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<usize>,
}

/// A failed request on the deprecated `/api/*` aliases: `{"error": "...", "position"?}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LegacyErrorBody {
    pub error: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<usize>,
}

/// `GET /api/v1/info`: what this engine is and what it can do.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Info {
    /// The engine's version (`0.1.0-beta.3`).
    pub version: String,
    /// [`API_VERSION`].
    pub api_version: String,
    /// `local`: one user on this machine. `server` arrives with authentication.
    pub mode: String,
    /// The route groups this engine serves, such as `runs`, `traces` or `admin`. A client checks
    /// for a capability before using a route that older engines lack.
    pub capabilities: Vec<String>,
}
