//! `orbit job run <yaml-path>` — schemaVersion 2 job entrypoint.
//!
//! Reads the YAML, routes through the two-pass loader, and dispatches via the
//! Phase 3 DAG executor.
//! orbit-core never names orbit-agent types — transport/session construction
//! lives below the boundary in `orbit_engine::job_executor`.

mod entry;
mod finalize;

pub use entry::V2JobRunResult;
pub(super) use finalize::seeded_resume_state;
pub(crate) use finalize::{V2RunFinalizationOptions, audit_terminal_outcome};
