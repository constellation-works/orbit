//! `orbit run` command implementation split across focused submodules.
//!
//! - `types` — `JobRunListParams` and `JobRunCancelResult` DTOs.
//! - `actions` — cancel/archive/delete flows and pipeline state marking.
//! - `query` — list/show/history entry points plus backend queries.
//! - `reconcile` — stale-run reconciliation, terminal timing repair, audit parsing.
//! - `owner` — process signalling, owner identity classification, liveness probes (Unix + shims).
//! - `delivery` — the bounded public commit/landing observation for one task.
//! - `conflict` — recording a terminal outcome that contradicts the one already persisted.
//! - `worker_limit` — adjusting a live auto drain's worker ceiling.
//! - `admissions_stop` — stopping new admissions on a live auto drain.
//! - `drain_cancel` — graceful and forced cancellation of a drain with in-flight leaves.
//! - `step_recovery` — authenticating executor-owned recovery before it mutates Git.
//! - `tests/*` — helpers and regression tests split by concern (actions, reconcile, owner, conflict).

mod actions;
mod admissions_stop;
mod conflict;
mod delivery;
mod drain_cancel;
mod owner;
mod projection;
mod query;
mod reconcile;
mod step_recovery;
mod types;
mod worker_limit;

/// A claimed worker conclusively exited without a recorded cancellation.
/// The reconciler cannot recover its exit signal once the process is gone.
pub(crate) const WORKER_TERMINATED_ERROR_CODE: &str = "worker_terminated";

#[cfg(test)]
mod tests;

#[cfg(unix)]
pub(crate) use actions::{CANCELLATION_WORKER_EXIT_AUDIT, active_cancellation_request};
pub use admissions_stop::{
    DrainAdmissionsStopChange, DrainAdmissionsStopRequest, DrainAdmissionsStopResult,
    RemainingDrainChild,
};
#[cfg(test)]
pub(crate) use conflict::TERMINAL_OUTCOME_CONFLICT_CODE;
pub(crate) use owner::running_run_has_verified_owner;
pub(crate) use owner::{RunOwnerLiveness, run_owner_liveness};
pub use projection::{
    ActivityInvocationEvidence, job_run_to_json, job_run_to_json_with_activity_provenance,
    run_error_step,
};
pub use types::{JobRunCancelResult, JobRunListParams, JobRunOrder, UnstoppedLeaf};
pub use worker_limit::{DrainWorkerLimitChange, DrainWorkerLimitRequest};
