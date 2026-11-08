//! Task commands and coordinated record writes.

mod add;
mod artifact_export;
pub mod artifact_put_result;
mod automation_port;
// Compatibility shim: the forecast lives above `workspace_auto`, but `lib.rs`
// and `orbit-web` still name these at this path. Retiring it means re-pointing
// them. `scripts/check-dependency-direction.sh` allows exactly these two lines.
pub use crate::application::backlog_forecast::BacklogForecastHistory;
#[cfg(feature = "test-support")]
pub use crate::application::backlog_forecast::COMPUTE_BUDGET_ENV;
pub(in crate::application) mod base_revision;
mod baseline_hold;
mod baseline_repair;
pub(crate) mod batch_prepare;
mod block_cause;
pub(crate) mod candidate_checkpoint;
pub(crate) use baseline_repair::BASELINE_RED_REPAIR_TAG;
pub(crate) mod blocked_recovery;
pub(crate) mod contention;
mod context_repair;
mod context_widening;
mod decision;
mod decision_channel;
use decision::model as decision_model;
use decision::raise as decision_raise;
pub(crate) use decision::rules as decision_rules;
mod desktop;
mod distributed_port;
pub(crate) mod escalation_triage_steps;
mod executor_host_os;
mod external_merge;
mod proposed;
pub use proposed::ProposedTaskInput;
mod provider_hold;
pub(crate) mod provider_limit;
pub(crate) use desktop::HandoffPullRequest;
mod final_recovery;
mod finding_verification;
mod grants;
mod handoff_claim;
mod helpers;
mod human_format;
mod job_port;
pub(crate) mod legacy_decisions;
mod lifecycle;
pub(crate) mod light_delivery;
mod lint;
mod list_result;
mod listing;
pub(crate) mod metadata;
mod mutation_proposals;
mod os_readout;
mod params;
mod paths;
pub(in crate::application) use paths::{CONTEXT_FILES_PRUNED_EVENT, pruned_selectors_from_note};
pub(crate) mod pilot;
mod post_deploy;
mod pr_closure;
mod precedent_decision_port;
pub mod precedent_provider;
mod pull_request_links;
mod query;
mod record_json;
mod records;
mod requeue;
mod resolves;
mod retained_candidate;
mod schedule;
pub(crate) use schedule::schedule_hold_in;
mod terminal_dates;
pub(in crate::application) use terminal_dates::latest_status_entry_at;
pub use terminal_dates::{DatedTerminalTask, TerminalTaskDates};
mod review_port;
pub(crate) use retained_candidate::RetainedCandidate;
mod ci_sweep_filing;
mod screened;
mod steering;
mod sweep_filing_port;
mod transient_infra;
mod transitions;
mod update;
pub(crate) mod validation_tools;
mod workspace_auto_port;
mod write_response;

/// The shared non-pruning footprint calculation. Task reads, projections,
/// reservations, status-derived locks, and the admission work that freezes a
/// claim's footprint all resolve declarations through it.
pub use crate::runtime::task::{DeclaredContextFiles, declared_context_files};
pub use artifact_export::TaskArtifactRead;
pub(in crate::application) use automation_port::{SourceGit, TaskAutomationPort};
pub(crate) use baseline_hold::refresh_baseline_holds_step;
pub use baseline_hold::{BASELINE_HOLD_REFRESH_JOB, BaselineHoldRefresh};
pub(in crate::application) use block_cause::{
    Anchor, BlockCause, SettledFailure, actor_kind, classify_block, entry_marks_run,
};
pub(crate) use blocked_recovery::{
    BACKSTOP_DECISIONS, BACKSTOP_LANE_CONTRACT, BlockedRecoveryPreparation, recovery_checkout_path,
};
pub use blocked_recovery::{
    BLOCKED_TASK_RECHECKED_EVENT, BLOCKED_TASK_RECOVERY_JOB, BLOCKED_TASK_TRIAGE_RESUMED_EVENT,
    BLOCKED_TASK_TRIAGED_EVENT, BlockEpisode, BlockSource, BlockedNextStep,
    BlockedRecheckSettlement, BlockedRecoveryInput, BlockedRecoveryTick, BlockedRecoveryView,
    BlockedTriageView, EpisodeDisposition, FinalRecoveryRecord, MAX_ACTIVE_BLOCKED_RECOVERIES,
    MAX_EPISODE_AGE_HOURS, RecheckKind, TriageBasis, TriageCandidacy, TriageEpisode, TriageTarget,
    episode_disposition, triage_candidacy,
};
pub use contention::{LockContentionHotspot, LockContentionReport};
pub use context_repair::{
    CONTEXT_REPAIR_SWEEP_STATUSES, ContextFileRestoration, ContextRepairCandidate,
};
pub use decision::DecisionRequestListing;
#[cfg(feature = "test-support")]
pub use decision::answer_checkpoint::with_decision_answer_checkpoint;
pub(crate) use decision::decision_request_json;
#[cfg(feature = "test-support")]
pub use decision_channel::dkim::test_support::with_decision_dkim_resolver;
pub use decision_channel::dkim::{DkimTxtResolver, DkimVerifyError, SystemDkimTxtResolver};
pub use decision_channel::{
    DECISION_CHANNEL_KEY_SECRET, DecisionChannelInfo, DecisionDigest, DecisionSubmitCaller,
    DecisionSubmitParams, MAX_DECISION_RAW_MESSAGE_BYTES,
};
pub use decision_model::{ModelAnswerApplied, ModelAnsweredDigestEntry};
#[cfg(feature = "test-support")]
pub use decision_raise::raise_checkpoint::with_decision_raise_checkpoint;
pub use decision_raise::{AgentDecisionRaise, AgentDecisionRaised};
pub use decision_rules::{
    ConsolidationAnswer, CorpusDecisionDocument, CorpusDecisionRule, CorpusDigest,
    CorpusRuleStatus, ProposedCorpusDocument, citable_decision_rules, consolidate_decision_corpus,
    corpus_content_digest, parse_decision_rules,
};
pub(in crate::application) use distributed_port::TaskDistributedPort;
pub(crate) use distributed_port::execution_claim_phase_label;
pub use escalation_triage_steps::{
    PILOT_WARNING_TRIAGED_EVENT, PilotWarningDecision, PilotWarningTriageRequest,
};
pub(crate) use escalation_triage_steps::{PilotWarningCandidacy, pilot_warning_candidacy};
pub(crate) use final_recovery::TERMINAL_REFUSAL_MARK;
pub use final_recovery::{
    FINAL_RECOVERY_REQUEUED_EVENT, FinalRecoveryCompletion, FinalRecoveryOutcome,
    FinalRecoveryRequest, FinalRecoveryRequeueBound, FinalRecoveryTaskRevision,
};
pub(crate) use finding_verification::{
    AnalyzedFinding, InvalidFinding, VerdictWrite, crew_named_by, finding_fingerprint,
    latest_verification,
};
pub use finding_verification::{
    FINDING_VERIFICATION_SCHEMA_VERSION, FindingEvidence, FindingRejection, FindingVerdict,
    FindingVerification, REVIEW_FINDING_TAGS, RejectionReason, VerdictConfidence, VerdictSource,
    VerifierVerdict, is_review_finding,
};
use job_port::CrewPools;
pub(in crate::application) use job_port::{NativePublicationIdentity, TaskJobPort};
pub use lint::{TaskLintFinding, TaskLintReport, TaskLintSeverity};
pub use list_result::{TaskListResultPolicy, TaskListResultProfile};
pub use listing::{
    TaskCandidateKey, TaskCandidateKeys, TaskCandidates, TaskListFilter, TaskListQuery, TaskPage,
    TaskRow,
};
pub(crate) use listing::{TaskEligibilityQuery, metadata_task};
pub use os_readout::TaskOsReadout;
pub(crate) use params::TaskRecordUpdateParams;
pub use params::{TaskAddParams, TaskUpdateParams};
pub use paths::ContextCreationAuthorization;
pub(crate) use pilot::pilot_assessable_status;
pub use pilot::task_pilot_warnings;
pub(crate) use pilot::{
    HostOperationalHold, MachineRequirement, NativeOsHold, NativeOsRequirement, NoDiffClosure,
    OperatorValidationHold, OperatorValidationRequirement, PILOT_VERIFIED_NO_DIFF,
    PilotAdmissionHold, VerifiedNoDiff, normalize_pilot_self_blockers,
    operator_validation_requirements, pilot_blocker_task_ids, warning_hold_findings,
};
pub(crate) use post_deploy::owed_criteria_json;
pub use post_deploy::{PostDeployOperatorRecord, PostDeployRecordRefusal};
pub(in crate::application) use precedent_decision_port::TaskPrecedentDecisionPort;
pub use provider_limit::{
    CrewProviderLimit, CrewUsagePressure, PressureWindow, ProviderLimitReading,
    ProviderLimitThreshold, ProviderLimitsView, ProviderUsageStatus, UngatedLaneLimit,
    UsagePressure, short_time,
};
pub use pull_request_links::PullRequestLinks;
pub(crate) use record_json::task_listing_field_json;
pub use record_json::{task_record_json, task_show_field_json, task_show_json};
pub(crate) use records::TaskRecords;
pub(in crate::application) use review_port::TaskReviewPort;
pub use screened::{ScreenedTaskWrite, TaskUpdateAuthority, TaskWriteScreen};
pub(in crate::application) use sweep_filing_port::TaskSweepFilingPort;
pub(crate) use sweep_filing_port::{
    AdmissionClosure, CiSweepAdmission, CiSweepAdmissionDecision, CiSweepFilingEvidence,
    CiSweepFilingSource, PromotionDecision,
};
pub use transient_infra::TransientInfraBlockedTask;
pub(crate) use validation_tools::positive_validation_tools;
pub(in crate::application) use workspace_auto_port::TaskWorkspaceAutoPort;
pub(crate) use write_response::task_reference_status_index;
pub use write_response::{
    DashboardPageReads, TaskWriteMetadata, TaskWriteResponsePolicy, active_task_triage_map,
    job_run_is_locally_navigable, link_pull_requests, mark_readiness_preparing,
    screened_task_write_response_json, summary_status_transitions,
    task_detail_with_dashboard_extras, task_fields_response_json,
    task_write_response_from_tool_answer, task_write_response_json, tasks_in_pilot_preparation,
    with_dashboard_extras,
};

pub(crate) use external_merge::settled_merge_commit;
pub use external_merge::{
    ExternalMergeBatchSettlement, ExternalMergeSettlement, ExternalMergeSettlementProfile,
    ExternalMergeSkippedTask, TASK_LANDING_REFUSED, TaskPullRequestLanding,
};
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

/// What a task write needs from `codebase` [ORB-22625]: the transition into
/// done records the landed delivery's diff stats. `codebase` owns the landed
/// rows and sits above `task`, so a direct call from here would climb into
/// it; `codebase` implements this port on [`OrbitRuntime`](crate::OrbitRuntime),
/// so composing the runtime binds it.
pub(in crate::application) trait TaskCodebasePort {
    /// On `task`'s transition into done from `previous_status`, record the
    /// diff stats of the landing its history's receipt names. Never fails
    /// the write: a missing commit or a store error is logged.
    fn record_landed_diff_stats_on_done(
        &self,
        previous_status: orbit_types::task::TaskStatus,
        task: &orbit_types::task::Task,
    );
}

#[cfg(test)]
mod tests;
