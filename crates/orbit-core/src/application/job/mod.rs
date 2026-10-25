mod activity_tool_requirements;
mod admission_port;
mod agent;
mod auto_concurrency_port;
mod automation_port;
pub(crate) mod catalog;
pub(crate) mod claimed;
pub(crate) mod crew_pools;
pub(crate) mod delivery;
mod drain_port;
mod exec;
mod explore_analytics;
mod final_recovery;
mod forge_hold_resume;
mod orphan_adoption;
mod pilot_submit;
pub(crate) mod pipeline;
pub(crate) mod pipeline_actions;
mod pr_repair;
mod resume;
mod review_hook;
mod run;
mod task_port;
mod upgrade_resume;

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

pub(crate) use activity_tool_requirements::drops_disallowed_requirements;
pub(in crate::application) use admission_port::JobAdmissionPort;
pub use agent::{
    AGENT_INVOKE_JOB_ID, AgentInvokeRequest, AgentInvokeResult, AgentInvokeSubmission,
    DEFAULT_AGENT_INVOKE_TIMEOUT_SECONDS, MAX_AGENT_INVOKE_TIMEOUT_SECONDS, agent_invoke_result,
};
pub use agent::{
    AgentInvocation, BoundedExploreAnswer, EXPLORE_JOB_ID, ExploreInvocationSummary,
    ExploreSubmission,
};
pub(in crate::application) use auto_concurrency_port::JobAutoConcurrencyPort;
pub(in crate::application) use automation_port::JobAutomationPort;
pub use catalog::CatalogReferenceLayer;
pub(crate) use catalog::{DEFAULT_JOB_FILES, seed_default_jobs};
pub use catalog::{JobCatalogEntry, JobCatalogFilter, job_catalog_to_json_with_last_run};
pub(in crate::application) use drain_port::{JobDrainPort, JobPullSettlementPort};
pub use exec::V2JobRunResult;
pub use explore_analytics::{
    EXPLORE_COMPARISON_SCHEMA_VERSION, ExploreCohort, ExploreComparison, ExploreCrewComparison,
};
pub(crate) use orphan_adoption::AdoptionAuthority;
pub use pilot_submit::{TaskPilotRefusal, TaskPilotSubmitError};
pub use pipeline::{
    PipelineInvokeResult, PipelineWaitEntry, PipelineWaitResult, PipelineWaitStatus,
    PipelineWorkerLogSnapshot, SubmissionKind, SubmissionNotices,
};
pub(crate) use resume::PublishedResume;
pub(in crate::application) use review_hook::ReviewAdmissionHook;
pub(crate) use run::CodebaseDeliveryScan;
pub(crate) use run::running_run_has_verified_owner;
pub use run::{
    ActivityInvocationEvidence, DrainAdmissionsStopChange, DrainAdmissionsStopOutcome,
    DrainAdmissionsStopRequest, DrainAdmissionsStopResult, DrainWindowExtensionChange,
    DrainWindowExtensionOutcome, DrainWindowExtensionRequest, DrainWorkerLimitChange,
    DrainWorkerLimitOutcome, DrainWorkerLimitRequest, JobRunCancelOutcome, JobRunCancelResult,
    JobRunListParams, JobRunListStates, JobRunOrder, RemainingDrainChild, RunDisplayStep,
    RunDisplaySteps, RunIncident, RunStepSource, UnstoppedChild, UnstoppedLeaf, fold_run_incidents,
    job_run_task_ids, job_run_to_json, job_run_to_json_with_activity_provenance, run_display_steps,
    run_error_step,
};
pub(crate) use run::{RunOwnerLiveness, drain_deadline, pull_window_closed, run_owner_liveness};
pub(crate) use run::{kept_candidate, kept_clause};
