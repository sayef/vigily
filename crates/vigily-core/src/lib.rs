//! # vigily-core
//!
//! The domain model of the vigily experiment tracker, independent of HTTP and Python:
//!
//! - [`model`]: runs, parameters, metric series, summaries and metric keys (`loss[val]`).
//! - [`store`]: the storage traits; [`MemoryStore`] for tests and [`Kv0Store`] (feature
//!   `kv0`, on by default) on Lance.
//! - [`catalog`]: the in-memory index the runs table is served from, refreshed incrementally.
//! - [`query`]: the run query language (`lr<0.01 loss[val].min<0.3 status:finished`).
//! - [`chart`]: x axes, min/max-envelope downsampling onto a shared x array, group aggregation.
//! - [`records`]: per-item records ([`records::RecordStore`]); [`evals`]: datasets, results, labels,
//!   feature samples, schedule runs, alerts and baselines on top of it.
//! - [`stats`] and [`drift`]: the tests and the behaviour- and data-drift verdicts.
//! - [`traces`]: LLM traces (spans, token usage, cost) and their payload policy.

pub mod catalog;
pub mod chart;
pub mod drift;
pub mod error;
pub mod evals;
#[cfg(feature = "kv0")]
mod kv0_store;
pub mod memory;
pub mod model;
pub mod query;
pub mod records;
pub mod stats;
pub mod store;
pub mod tokens;
pub mod traces;

pub use catalog::{Catalog, Column, RunEntry, Snapshot};
pub use error::{Error, Result};
#[cfg(feature = "kv0")]
pub use kv0_store::{Kv0Options, Kv0Store};
pub use memory::MemoryStore;
pub use model::*;
pub use query::Query;
pub use records::{MemoryRecords, RecordStore};
pub use store::{now_micros, PurgeSummary, RunWriter, Store, STALE_AFTER};

use std::future::Future;
use std::sync::{Arc, OnceLock};

/// Runs `fut` to completion from synchronous code (the Python SDK, the CLI's short commands) on a
/// process-wide runtime.
pub fn block_on<F: Future>(fut: F) -> F::Output {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("vigily")
            .enable_all()
            .build()
            .expect("failed to start the vigily runtime")
    })
    .block_on(fut)
}

/// Opens the repository at `location`: `memory` for a throwaway in-memory store, otherwise a
/// directory or object-store URI opened with kv0.
pub async fn open_store(location: &str) -> Result<Arc<dyn Store>> {
    if location == "memory" {
        return Ok(Arc::new(MemoryStore::new()));
    }
    #[cfg(feature = "kv0")]
    {
        // Migrate the shared records table of earlier builds before anything else opens the
        // repository (a no-op once done).
        records::Kv0Records::open(location).await?;
        let s = Kv0Store::open(location, Kv0Options::default()).await?;
        Ok(Arc::new(s))
    }
    #[cfg(not(feature = "kv0"))]
    {
        Err(Error::InvalidArgument(format!(
            "{location}: this build has no storage engine (feature `kv0`); only `memory` works"
        )))
    }
}

/// Opens the record store of the repository at `location` (see [`open_store`]).
pub async fn open_records(location: &str) -> Result<Arc<dyn RecordStore>> {
    if location == "memory" {
        return Ok(Arc::new(MemoryRecords::new()));
    }
    #[cfg(feature = "kv0")]
    {
        Ok(Arc::new(records::Kv0Records::open(location).await?))
    }
    #[cfg(not(feature = "kv0"))]
    {
        Err(Error::InvalidArgument(format!(
            "{location}: this build has no storage engine (feature `kv0`); only `memory` works"
        )))
    }
}
