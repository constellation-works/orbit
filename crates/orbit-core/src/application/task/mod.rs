//! Task commands and coordinated record writes.

mod add;
mod artifact_export;
mod automation_port;
mod baseline_hold;
mod baseline_repair;
pub(crate) use baseline_repair::BASELINE_RED_REPAIR_TAG;
pub(crate) mod blocked_recovery;
pub(crate) mod contention;
mod context_repair;
mod context_widening;
mod desktop;
pub(crate) mod escalation_triage_steps;
mod external_merge;
mod proposed;
pub use proposed::ProposedTaskInput;
mod provider_hold;
pub(crate) mod provider_limit;
pub(crate) use desktop::HandoffPullRequest;
mod final_recovery;
mod finding_verification;
pub(crate) mod finding_verification_steps;
mod helpers;
pub(crate) mod legacy_decisions;
mod lifecycle;
pub(crate) mod light_delivery;
mod lint;
mod listing;
mod os_readout;
mod params;
mod paths;
pub(crate) mod pilot;
mod post_deploy;
mod pr_closure;
mod pull_request_links;
mod query;
mod record_json;
mod records;
mod requeue;
mod resolves;
mod retained_candidate;
pub(crate) use retained_candidate::RetainedCandidate;
mod screened;
mod steering;
mod transient_infra;
mod transitions;
mod update;
pub(crate) mod validation_tools;
mod write_response;

/// The shared non-pruning footprint calculation. Task reads, projections,
/// reservations, status-derived locks, and the admission work that freezes a
/// claim's footprint all resolve declarations through it.
pub use crate::runtime::task::{DeclaredContextFiles, declared_context_files};
pub use artifact_export::TaskArtifactRead;
pub(in crate::application) use automation_port::TaskAutomationPort;
pub(crate) use baseline_hold::refresh_baseline_holds_step;
pub use baseline_hold::{BASELINE_HOLD_REFRESH_JOB, BaselineHoldRefresh};
pub(crate) use blocked_recovery::{
    BACKSTOP_DECISIONS, BACKSTOP_LANE_CONTRACT, BlockedRecoveryPreparation, recovery_checkout_path,
};
pub use blocked_recovery::{
    BLOCKED_TASK_RECOVERY_JOB, BLOCKED_TASK_TRIAGED_EVENT, BlockEpisode, BlockSource,
    BlockTriageDecision, BlockTriageRequest, BlockedNextStep, BlockedRecoveryInput,
    BlockedRecoveryTick, BlockedRecoveryView, BlockedTriageView, EpisodeDisposition,
    FinalRecoveryRecord, MAX_ACTIVE_BLOCKED_RECOVERIES, MAX_EPISODE_AGE_HOURS, TriageBasis,
    TriageCandidacy, TriageEpisode, TriageOption, episode_disposition, triage_candidacy,
};
pub use contention::{LockContentionHotspot, LockContentionReport};
pub use context_repair::{
    CONTEXT_REPAIR_SWEEP_STATUSES, ContextFileRestoration, ContextRepairCandidate,
};
pub use escalation_triage_steps::{
    PILOT_WARNING_TRIAGED_EVENT, PilotWarningDecision, PilotWarningTriageRequest,
};
pub use final_recovery::{
    FINAL_RECOVERY_REQUEUED_EVENT, FinalRecoveryCompletion, FinalRecoveryOutcome,
    FinalRecoveryRequest, FinalRecoveryRequeueBound, FinalRecoveryTaskRevision,
};
pub use finding_verification::{
    FINDING_VERIFICATION_SCHEMA_VERSION, FindingEvidence, FindingRejection, FindingSeverity,
    FindingVerdict, FindingVerification, REVIEW_FINDING_TAGS, RejectionReason, VerdictConfidence,
    VerifierReport, VerifierVerdict, is_review_finding,
};
pub(crate) use finding_verification::{
    UNVERIFIED_REVIEW_FINDING, VERIFIABLE_STATUSES, VerdictWrite, VerifierAssignment,
    finding_fingerprint, latest_verification, needs_verification, next_tier,
};
pub use lint::{TaskLintFinding, TaskLintReport, TaskLintSeverity};
pub use listing::{
    TaskCandidateKey, TaskCandidateKeys, TaskCandidates, TaskListFilter, TaskListQuery, TaskPage,
    TaskRow,
};
pub(crate) use listing::{TaskEligibilityQuery, metadata_task};
pub use os_readout::TaskOsReadout;
pub(crate) use params::TaskRecordUpdateParams;
pub use params::{TaskAddParams, TaskUpdateParams};
pub use paths::ContextCreationAuthorization;
pub use pilot::task_pilot_warnings;
pub(crate) use pilot::{
    HostOperationalHold, MachineRequirement, NativeOsHold, NativeOsRequirement, NoDiffClosure,
    OperatorValidationHold, OperatorValidationRequirement, PILOT_VERIFIED_NO_DIFF,
    PilotAdmissionHold, VerifiedNoDiff, normalize_pilot_self_blockers,
    operator_validation_requirements, pilot_blocker_task_ids, warning_hold_findings,
};
pub(crate) use post_deploy::owed_criteria_json;
pub use provider_limit::{
    CrewProviderLimit, CrewUsagePressure, PressureWindow, ProviderLimitReading,
    ProviderLimitThreshold, ProviderLimitsView, ProviderUsageStatus, UngatedLaneLimit,
    UsagePressure, short_time,
};
pub use pull_request_links::PullRequestLinks;
pub use record_json::{task_record_json, task_show_field_json, task_show_json};
pub(crate) use records::TaskRecords;
pub use screened::{ScreenedTaskWrite, TaskUpdateAuthority, TaskWriteScreen};
pub use transient_infra::TransientInfraBlockedTask;
pub(crate) use validation_tools::positive_validation_tools;
pub(crate) use write_response::task_reference_status_index;
pub use write_response::{
    TaskWriteMetadata, TaskWriteResponsePolicy, job_run_is_locally_navigable, link_pull_requests,
    mark_readiness_preparing, screened_task_write_response_json, summary_status_transitions,
    task_detail_with_dashboard_extras, task_fields_response_json,
    task_write_response_from_tool_answer, task_write_response_json, tasks_in_pilot_preparation,
    with_dashboard_extras,
};

pub use external_merge::ExternalMergeSettlement;
pub(crate) use external_merge::settled_merge_commit;
pub(crate) use helpers::{SYSTEM_ACTOR_LABEL, TaskAttributionInput, assemble_task_attribution};
pub use lifecycle::{
    EXTERNAL_MERGE_SETTLED_EVENT, task_status_transition_allowed,
    task_status_transition_required_field,
};
pub(crate) use lifecycle::{
    ensure_completion_run_stopped, ensure_post_deploy_criteria_met, ensure_task_has_execution_plan,
    in_progress_transition_requires_plan, is_operator_rescue_close,
};
pub(crate) use paths::context_workspace_root;
pub(crate) use resolves::pull_request_delivery;
pub(crate) use update::admit_agent_artifact;

#[cfg(test)]
mod tests;
