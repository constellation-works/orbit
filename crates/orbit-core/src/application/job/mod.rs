pub(crate) mod agent_invoke;
pub(crate) mod catalog;
pub(crate) mod catalog_layers;
pub(crate) mod claimed;
pub(crate) mod crew_pools;
pub(crate) mod delivery;
mod exec;
pub(crate) mod pipeline;
mod resume;
mod run;

#[cfg(test)]
mod tests;

/// Report a failed follow-up (diagnostic step, audit row, cancellation) that
/// must not replace the outcome the caller is already returning. Discarding
/// the result would leave the run without the record and without a trace of
/// why.
pub(crate) fn log_best_effort<T>(
    operation: &'static str,
    run_id: &str,
    result: Result<T, orbit_common::OrbitError>,
) {
    if let Err(error) = result {
        tracing::warn!(
            target: "orbit.core.job_run",
            run_id,
            operation,
            error = %error,
            "best-effort run bookkeeping failed",
        );
    }
}

pub use agent_invoke::{
    AGENT_INVOKE_JOB_ID, AgentInvokeRequest, AgentInvokeResult, AgentInvokeSubmission,
    DEFAULT_AGENT_INVOKE_TIMEOUT_SECONDS, MAX_AGENT_INVOKE_TIMEOUT_SECONDS, agent_invoke_result,
};
pub(crate) use catalog::{DEFAULT_JOB_FILES, seed_default_jobs};
pub use catalog::{JobCatalogEntry, JobCatalogFilter};
pub use catalog_layers::CatalogReferenceLayer;
pub use exec::V2JobRunResult;
pub use pipeline::{
    PipelineInvokeResult, PipelineWaitEntry, PipelineWaitResult, PipelineWorkerLogSnapshot,
};
#[cfg(test)]
pub(crate) use run::TERMINAL_OUTCOME_CONFLICT_CODE;
pub(crate) use run::running_run_has_verified_owner;
pub use run::{
    ActivityInvocationEvidence, DrainAdmissionsStopChange, DrainAdmissionsStopRequest,
    DrainAdmissionsStopResult, DrainWorkerLimitChange, DrainWorkerLimitRequest, JobRunCancelResult,
    JobRunListParams, JobRunOrder, RemainingDrainChild, UnstoppedChild, UnstoppedLeaf,
    job_run_to_json, job_run_to_json_with_activity_provenance, run_error_step,
};
pub(crate) use run::{RunOwnerLiveness, run_owner_liveness};
