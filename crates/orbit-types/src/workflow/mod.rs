//! Domain contracts for this Orbit types module.

pub mod activity_job;
mod auto_task;
mod child_dispatch;
mod error;
mod executor_def;
mod final_recovery;
pub mod handoff;
mod host_evidence;
mod job;
mod reconciliation;
mod review;
mod review_evidence;
mod routine;
mod run;
mod ship;
mod skill;
pub use error::WorkflowError;

#[cfg(test)]
mod tests;

pub use activity_job::{
    AUDIT_ENVELOPE_SCHEMA_VERSION, ActivityToolDenyPolicy, ActivityToolPolicyMode, ActivityV2,
    ActivityV2Spec, AgentLoopSpec, BackoffStrategy, BranchOutcome, CODEX_LEAST_RESTRICTIVE_SANDBOX,
    CODEX_PROVIDER_SANDBOX_MODES, CoreDeterministicAction, DEFAULT_PROVIDER_SANDBOX,
    DeterministicAction, DeterministicSpec, EngineDeterministicAction, FanInSpec, FanOutBlock,
    JobActivityRoles, JobKind, JobTaskDelivery, JobV2, JobV2Step, JobV2StepBody, JoinMode,
    LoopBlock, OnDenial, ParallelBlock, PipelineRef, Provider, ProviderAlias, ProviderDeprecation,
    ProviderDiagnostic, ProviderEntryPoint, ProviderIdentity, ProviderParseError,
    ProviderResolution, ProviderResolveRequest, ProviderSource, RETIRED_BACKEND_MIGRATION,
    RetiredAgentBackend, RetiredFeatureError, RetrySpec, SchemaHeader, TargetRef, TargetStep,
    ToolAllowlistError, V2_DENIAL_EVENT_TYPES, V2_EVENT_TYPE_FS_CALL_DENIED,
    V2_EVENT_TYPE_STEP_DENIED, V2_EVENT_TYPE_TOOL_DENIED,
    V2_INTENTIONALLY_EMPTY_TOOL_WILDCARD_ROOTS, V2_TOOL_WILDCARD_ROOTS, V2AuditEnvelope,
    V2AuditEvent, V2AuditEventKind, activity_tool_policy_deprecation, admit_provider_sandbox_mode,
    check_retired_backend_value, format_provider_sandbox, is_least_restrictive_provider_sandbox,
    least_restrictive_provider_sandbox, least_restrictive_provider_sandbox_warning,
    parse_provider_sandbox_label, provider_sandbox_modes, tool_allowed,
    tools_allowed_by_disallow_list, validate_activity_tool_allowlist,
    validate_activity_tool_allowlist_against_registered_tools, validate_job_retired_sessions,
    validate_tool_allowlist, validate_tool_allowlist_against_registered_tools,
};
pub use auto_task::{
    AUTO_TASK_SCHEMA_VERSION, AUTO_TASK_TAG_PREFIX, AutoTaskCursor, AutoTaskCursorState,
    AutoTaskDefinition, AutoTaskPendingClaim, AutoTaskSchedule, AutoTaskSkipRecord,
    AutoTaskTemplate, DedupePolicy, MAX_AUTO_TASK_INTERVAL_MINUTES, SWEEP_CURSOR_ARTIFACT,
    SWEEP_CURSOR_SCHEMA_VERSION, SkipIfUnchanged, SweepCursorRecord, SweepCursorSelector,
    auto_task_tag, is_valid_auto_task_name,
};
pub use child_dispatch::{
    ChildCancellation, ChildCancellationPolicy, ChildDispatch, ChildDispatchPhase,
};
pub use executor_def::{
    ExecutorDef, ExecutorSandboxKind, ExecutorType, ModelPairOverride, StdoutFormat,
};
pub use final_recovery::{
    FINAL_RECOVERY_ACTIVITY, FINAL_RECOVERY_CREWS_KEY, FinalRecoveryDecision,
    MAX_DECISION_TEXT_CHARS,
};
pub use host_evidence::{
    EvidenceHostOs, HostEvidenceReason, HostEvidenceRecord, HostEvidenceRefusal,
    HostSandboxCommand, judge_host_test_output,
};
pub use job::{
    AgentResponseEnvelope, AgentRunError, Job, JobRun, JobRunStartOutcome, JobRunState, JobRunStep,
    JobRunTrigger, JobRunTriggerKind, JobScheduleState, JobStep, JobTargetType,
    KnowledgeRunMetrics, RunEvent, RunStateUpdate, StepCondition, default_job_max_active_runs,
    default_max_iterations, default_retry_backoff_seconds,
};
pub use reconciliation::{
    BaselineDisposition, BaselineRemediationCheck, REVIEW_RECONCILIATION_ADMISSION_KEY,
    REVIEW_RECONCILIATION_JOB, REVIEW_RECONCILIATION_SCHEMA_VERSION, ReconciledCommand,
    ReconciledCommandRun, ReconciledExecution, ReconciledPullRequest, ReconciledReview,
    ReconciledValidation, ReconciliationAdmission, ReconciliationAttempt, ReconciliationBinding,
    ReconciliationCommandSource, ReconciliationContract, ReconciliationLog, ReconciliationOutcome,
    ReviewReconciliation, run_input_declares_review_reconciliation,
    strip_review_reconciliation_admission,
};
pub use review::{
    CommitIdentity, DEFAULT_REVIEW_MINUTES, FindingDisposition, LandingTransformation,
    NegativeControl, REVIEW_ADMISSION_KEY, REVIEW_BASELINE_ARTIFACT, REVIEW_CONTRACT_VERSION,
    REVIEW_GATE_ARTIFACT, REVIEW_MANIFEST_ARTIFACT, REVIEW_REPORT_ARTIFACT,
    REVIEW_REPORT_HISTORY_ARTIFACT, REVIEW_REPORT_HISTORY_LIMIT, REVIEW_REPORT_HISTORY_VERSION,
    RecordGap, RetainedObligation, RetiredValidation, ReviewAdmission, ReviewAssurance,
    ReviewAttempt, ReviewAttemptState, ReviewBaselineClaim, ReviewBudget, ReviewCertificate,
    ReviewConsumption, ReviewFinding, ReviewInvalidation, ReviewLanding, ReviewLedger,
    ReviewManifest, ReviewReport, ReviewReportHistory, ReviewReportRevision, ReviewReservation,
    ReviewResetDecision, ReviewTiming, ReviewValidation, ReviewVerdict, ReviewerIdentity,
    ReviewerInvocation, ReviewerInvocationEvent, ValidationOutcome, ValidationRole, record_gap,
    seconds_between,
};
pub use review_evidence::{
    HostEvidenceRule, REVIEW_EVIDENCE_HOLD_ARTIFACT, REVIEW_EVIDENCE_RECEIVED_EVENT,
    ReviewEvidenceCarried, ReviewEvidenceHold, ReviewEvidenceKind, ReviewEvidenceRequirement,
    ReviewEvidenceRerequestReason, ReviewExternalEvidence, owed_requirements,
};
pub use routine::{
    MissedRunPolicy, OverlapPolicy, ROUTINE_SCHEMA_VERSION, RoutineDefinition, RoutinePolicy,
    RoutineRetries, RoutineTarget, RoutineTrigger,
};
pub use run::{
    ActivityCrewDraw, ActivityCrewPoolMember, AgentBlocker, BASELINE_RED_ERROR_CODE,
    BASELINE_RED_HOLD_EVENT, BASELINE_RED_MARKER, BaselineRedHold, ClaimFailureClass,
    CommitObservation, CommitObservationStatus, CrewExclusion, CrewExclusionSource,
    DeliveryEvidenceGap, DeliveryEvidenceProvenance, DrainAdmissionPass, DrainAdmissionsStop,
    DrainApprovalReport, DrainCancelRequest, DrainCapacity, DrainWaitingTask, DrainWorkerLimit,
    FORGE_UNAVAILABLE_ERROR_CODE, FORGE_UNAVAILABLE_EXPIRED_EVENT, FORGE_UNAVAILABLE_MARKER,
    FailureActivityCheckpoint, FinalRecoveryCheckpoint, FinalRecoveryKey,
    FinalRecoveryObservedTask, FinalRecoveryRepairCommit, ForgeUnavailableHold, LandingMethod,
    LandingObservation, LandingObservationStatus, OWNER_ROUTE_UNAVAILABLE_ERROR_CODE,
    OWNER_ROUTE_UNAVAILABLE_MARKER, PROVIDER_CAPACITY_ERROR_CODE, PROVIDER_CAPACITY_MARKER,
    PROVIDER_FAILURE_HOLD_EVENT, PROVIDER_FAILURE_HOLD_MARKER, PROVIDER_REFUSAL_ERROR_CODE,
    PROVIDER_REFUSAL_MARKER, PROVIDER_UNAVAILABLE_ERROR_CODE, PROVIDER_UNAVAILABLE_MARKER,
    PipelineState, ProviderFailureClass, ProviderFailureHold, PullCrewPreflight, PullSinglePass,
    RUN_DELIVERY_EVIDENCE_SOURCE, RUN_DELIVERY_SCHEMA_VERSION, ResourcePressure, ResourceThrottle,
    RunDeliveryObservation, RunDeliveryStatus, RunIdRole, TASK_BLOCKED_BY_AGENT_ERROR_CODE,
    TASK_BLOCKED_BY_AGENT_EVENT, TASK_BLOCKED_BY_AGENT_MARKER, TRANSIENT_FAILURE_ERROR_CODE,
    TRANSIENT_FAILURE_MARKER, TaskCancellationPolicy, VALIDATION_ENVIRONMENT_ERROR_CODE,
    VALIDATION_ENVIRONMENT_MARKER, agent_blocker_from_output, failed_provider,
    is_baseline_red_failure, is_forge_unavailable, is_owner_route_unavailable,
    is_provider_capacity_exhausted, is_provider_failure, is_provider_refusal,
    is_provider_unavailable, is_task_blocked_by_agent, is_validation_environment_failure,
    provider_failure_text, run_id_candidate, run_id_minute_stem, run_id_role,
    task_blocked_by_agent_kind, task_blocked_by_agent_message,
};
pub use ship::{CompletionPolicy, ShipMode, resolved_ship_mode};
pub use skill::Skill;

pub mod automation;
