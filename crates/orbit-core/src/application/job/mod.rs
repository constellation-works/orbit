pub(crate) mod scripts;
pub(crate) use scripts::{attach as script_attach, outputs as script_outputs, step as script_step};
mod activity_tool_requirements;
mod admission_port;
mod agent;
mod auto_concurrency_port;
mod automation_port;
pub(crate) mod catalog;
pub(crate) mod claimed;
pub(crate) mod crew_pools;
pub(crate) mod delivery;
pub mod delivery_attention;
mod drain_leaf_port;
mod drain_port;
mod exec;
mod experiment_port;
mod final_recovery;
mod forge_hold_resume;
mod orphan_adoption;
mod pilot_submit;
pub(crate) mod pipeline;
pub(crate) mod pipeline_actions;
mod pr_repair;
mod resume;
mod review_hold;
mod review_hook;
mod run;
#[cfg(feature = "test-support")]
pub use run::with_run_root_cause_write_hook;
mod run_stage;
mod run_worktree;
mod task_port;
mod triage_submit;
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

#[cfg(test)]
pub(crate) use activity_tool_requirements::set_grant_classification_override;
pub(crate) use activity_tool_requirements::{
    DisallowGrant, IMPLEMENTING_ACTIVITY, agent_tool_grant_classification, classify_disallow_grant,
    drops_disallowed_requirements, granted_tool_refusal, implementing_activity_disallow_list,
    unliftable_required_tool_error,
};
pub(in crate::application) use admission_port::JobAdmissionPort;
pub use agent::{AgentInvokeResult, agent_invoke_result};
pub(in crate::application) use auto_concurrency_port::JobAutoConcurrencyPort;
pub(in crate::application) use automation_port::JobAutomationPort;
pub use catalog::CatalogReferenceLayer;
pub(crate) use catalog::{DEFAULT_JOB_FILES, seed_default_jobs};
pub use catalog::{JobCatalogEntry, JobCatalogFilter, job_catalog_to_json_with_last_run};
pub use crew_pools::{TaskCrewBackfill, TaskCrewChange};
pub(in crate::application) use drain_leaf_port::JobDrainLeafPort;
pub(in crate::application) use drain_port::{JobDrainPort, JobPullSettlementPort};
pub use exec::V2JobRunResult;
pub(in crate::application) use experiment_port::JobExperimentPort;
pub(crate) use orphan_adoption::AdoptionAuthority;
pub use pilot_submit::{
    TaskPilotRefusal, TaskPilotSubmission, TaskPilotSubmissionDashboardView, TaskPilotSubmitError,
};
pub use pipeline::{
    AutoDrainSubmissionProfile, AutoDrainSubmissionReceipt, AutoDrainSubmissionSource,
    PipelineInvokeResult, PipelineWaitEntry, PipelineWaitResult, PipelineWaitStatus,
    PipelineWorkerLogSnapshot, ShipModeResolutionProfile, SubmissionKind, SubmissionNotices,
};
pub use pr_repair::{TaskAutoFixError, TaskAutoFixRefusal};
pub(crate) use resume::PublishedResume;
pub use review_hold::{ReviewHold, ReviewHoldKind};
pub(in crate::application) use review_hook::ReviewAdmissionHook;
pub(crate) use run::CodebaseDeliveryScan;
pub(crate) use run::running_run_has_verified_owner;
pub use run::{
    ActivityInvocationEvidence, BOUNDED_RUN_ANSWER_MAX_BYTES, BOUNDED_RUN_STEP_LIMIT,
    DrainAdmissionsStopChange, DrainAdmissionsStopOutcome, DrainAdmissionsStopRequest,
    DrainAdmissionsStopResult, DrainWindowExtensionChange, DrainWindowExtensionOutcome,
    DrainWindowExtensionRequest, DrainWorkerLimitChange, DrainWorkerLimitChangeProfile,
    DrainWorkerLimitOutcome, DrainWorkerLimitRequest, JOB_FAILURE_STREAK_WARN, JobFailureStreak,
    JobRunCancelOutcome, JobRunCancelResult, JobRunCancelResultProfile, JobRunHolder,
    JobRunListParams, JobRunListStates, JobRunOrder, JobRunProjectionEvidence,
    JobRunProjectionProfile, RecordedJobRun, RemainingDrainChild, RunDisplayStep, RunDisplaySteps,
    RunFailureCause, RunFailureContext, RunFailureContextProfile, RunFailureContextWarning,
    RunFailureEvidence, RunFailureRead, RunIncident, RunStepSource, UnstoppedChild, UnstoppedLeaf,
    add_bounded_display_steps_to_json, bound_bounded_agent_invocation_answer,
    display_steps_with_agent_execution_to_json, fold_run_incidents, job_run_task_ids,
    job_run_to_json, job_run_to_json_with_activity_provenance, project_job_run, run_display_steps,
    run_error_step, run_execution_progress_to_json, run_upgrade_paused,
    trim_full_show_agent_invocation_answer,
};
pub(crate) use run::{
    RunOwnerLiveness, drain_deadline, pull_window_closed, run_owner_liveness,
    running_run_owner_is_stale,
};
pub(crate) use run::{kept_candidate, kept_clause};
pub use triage_submit::{EscalationTriageRefusal, EscalationTriageSubmitError};
