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
//! - `queued_cancel` — cancelling a run only while it is still queued, refusing one that started.
//! - `interrupted_review` — keeping the repair of a review a stopped run was inside.
//! - `child_timeout` — cancelling a child whose parent's wait expired, keeping its candidate.
//! - `completion_audit` — the completion audit the stop, worker-ceiling and extension controls share.
//! - `root_cause` — typed root causes of cascaded failures, folding a listing by incident, and the
//!   run-failure observation query run views share.
//! - `step_recovery` — authenticating executor-owned recovery before it mutates Git.
//! - `tests/*` — helpers and regression tests split by concern (actions, reconcile, owner, conflict).

mod actions;
mod admissions_stop;
mod build_budget;
mod child_timeout;
mod completion_audit;
mod conflict;
mod deferral;
mod delivery;
mod display_steps;
mod drain_cancel;
mod failure_streak;
mod interrupted_review;
mod owner;
mod projection;
mod query;
mod queued_cancel;
mod reconcile;
mod recorded;
mod root_cause;
#[cfg(feature = "test-support")]
pub use root_cause::with_run_root_cause_write_hook;
mod step_recovery;
mod types;
mod window_extension;
mod worker_limit;

/// A claimed worker conclusively exited without a recorded cancellation.
/// The reconciler cannot reap its exit signal once the process is gone; it
/// names the exit the worker's supervisor recorded, if any [ORB-17179].
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
pub(crate) use delivery::CodebaseDeliveryScan;
pub use display_steps::{RunDisplayStep, RunDisplaySteps, RunStepSource, run_display_steps};
pub use failure_streak::{JOB_FAILURE_STREAK_WARN, JobFailureStreak};
pub(crate) use owner::running_run_has_verified_owner;
pub(crate) use owner::running_run_owner_is_stale;
pub(crate) use owner::{RunOwnerLiveness, run_owner_liveness};
pub use projection::{
    ActivityInvocationEvidence, BOUNDED_RUN_ANSWER_MAX_BYTES, BOUNDED_RUN_STEP_LIMIT,
    JobRunListStates, JobRunProjectionEvidence, JobRunProjectionProfile,
    add_bounded_display_steps_to_json, bound_bounded_agent_invocation_answer,
    display_steps_with_agent_execution_to_json, job_run_task_ids, job_run_to_json,
    job_run_to_json_with_activity_provenance, project_job_run, run_error_step,
    run_execution_progress_to_json, trim_full_show_agent_invocation_answer,
};
pub use recorded::{JobRunHolder, RecordedJobRun};
pub use root_cause::{
    RunFailureCause, RunFailureContext, RunFailureContextProfile, RunFailureContextWarning,
    RunFailureEvidence, RunFailureRead, RunIncident, fold_run_incidents, run_upgrade_paused,
};
pub use types::{
    JobRunCancelOutcome, JobRunCancelResult, JobRunCancelResultProfile, JobRunListParams,
    JobRunOrder, UnstoppedChild, UnstoppedLeaf,
};
pub use window_extension::{
    DrainWindowExtensionChange, DrainWindowExtensionOutcome, DrainWindowExtensionRequest,
};
pub(crate) use window_extension::{drain_deadline, pull_window_closed};
pub use worker_limit::{
    DrainWorkerLimitChange, DrainWorkerLimitChangeProfile, DrainWorkerLimitOutcome,
    DrainWorkerLimitRequest,
};
