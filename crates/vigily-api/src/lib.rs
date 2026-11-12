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
