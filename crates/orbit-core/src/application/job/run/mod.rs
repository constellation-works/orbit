//! `orbit run` command implementation split across focused submodules.
//!
//! - `types` — `JobRunListParams` and `JobRunCancelResult` DTOs.
//! - `actions` — cancel/archive/delete flows and pipeline state marking.
//! - `query` — list/show/history entry points plus backend queries.
//! - `reconcile` — stale-run reconciliation, terminal timing repair, audit parsing.
//! - `owner` — process signalling, owner identity classification, liveness probes (Unix + shims).
//! - `delivery` — the bounded public commit/landing observation for one task.
//! - `display_steps` — the one rule for which steps a run view renders and how an audit step reads.
//! - `conflict` — recording a terminal outcome that contradicts the one already persisted.
//! - `deferral` — recording a run that deferred its work instead of failing, for Incidents.
//! - `worker_limit` — adjusting a live auto drain's worker ceiling.
//! - `admissions_stop` — stopping new admissions on a live auto drain.
//! - `window_extension` — lengthening a live drain's window.
//! - `drain_cancel` — graceful and forced cancellation of a drain with in-flight leaves.
//! - `interrupted_review` — keeping the repair of a review a stopped run was inside.
//! - `child_timeout` — cancelling a child whose parent's wait expired, keeping its candidate.
//! - `root_cause` — typed root causes of cascaded failures, and folding a listing by incident.
//! - `step_recovery` — authenticating executor-owned recovery before it mutates Git.
//! - `tests/*` — helpers and regression tests split by concern (actions, reconcile, owner, conflict).

mod actions;
mod admissions_stop;
mod build_budget;
mod child_timeout;
mod conflict;
mod deferral;
mod delivery;
mod delivery_execution;
mod display_steps;
mod drain_cancel;
mod interrupted_review;
mod owner;
mod projection;
mod query;
mod reconcile;
mod root_cause;
mod step_recovery;
mod types;
mod window_extension;
mod worker_limit;

/// A claimed worker conclusively exited without a recorded cancellation.
/// The reconciler cannot recover its exit signal once the process is gone.
pub(crate) const WORKER_TERMINATED_ERROR_CODE: &str = "worker_terminated";

#[cfg(test)]
mod tests;

#[cfg(unix)]
pub(crate) use actions::{CANCELLATION_WORKER_EXIT_AUDIT, active_cancellation_request};
pub use admissions_stop::{
    DrainAdmissionsStopChange, DrainAdmissionsStopOutcome, DrainAdmissionsStopRequest,
    DrainAdmissionsStopResult, RemainingDrainChild,
};
pub(crate) use child_timeout::{kept_candidate, kept_clause};
pub use display_steps::{RunDisplayStep, RunDisplaySteps, RunStepSource, run_display_steps};
pub(crate) use owner::running_run_has_verified_owner;
pub(crate) use owner::{RunOwnerLiveness, run_owner_liveness};
pub use projection::{
    ActivityInvocationEvidence, job_run_task_ids, job_run_to_json,
    job_run_to_json_with_activity_provenance, run_error_step,
};
pub use root_cause::{RunIncident, fold_run_incidents};
pub use types::{
    JobRunCancelOutcome, JobRunCancelResult, JobRunListParams, JobRunOrder, UnstoppedChild,
    UnstoppedLeaf,
};
pub use window_extension::{
    DrainWindowExtensionChange, DrainWindowExtensionOutcome, DrainWindowExtensionRequest,
};
pub(crate) use window_extension::{drain_deadline, pull_window_closed};
pub use worker_limit::{DrainWorkerLimitChange, DrainWorkerLimitOutcome, DrainWorkerLimitRequest};
