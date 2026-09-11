//! Domain contracts for this Orbit types module.

pub mod activity_job;
mod auto_task;
mod child_dispatch;
mod error;
mod executor_def;
mod job;
pub mod operation;
mod review;
mod routine;
mod run_id;
mod run_state;
mod ship;
mod skill;
pub use error::WorkflowError;

#[cfg(test)]
mod tests;

pub use activity_job::{
    AUDIT_ENVELOPE_SCHEMA_VERSION, ActivityV2, ActivityV2Spec, AgentLoopSpec, BackoffStrategy,
    BranchOutcome, CoreDeterministicAction, DeterministicAction, DeterministicSpec,
    EngineDeterministicAction, FanInSpec, FanOutBlock, JobActivityRoles, JobKind, JobV2, JobV2Step,
    JobV2StepBody, JoinMode, LoopBlock, OnDenial, ParallelBlock, PipelineRef, Provider,
    ProviderAlias, ProviderDeprecation, ProviderDiagnostic, ProviderEntryPoint, ProviderIdentity,
    ProviderParseError, ProviderResolution, ProviderResolveRequest, ProviderSource,
    RETIRED_BACKEND_MIGRATION, RetiredAgentBackend, RetiredFeatureError, RetrySpec, SchemaHeader,
    TargetRef, TargetStep, ToolAllowlistError, V2_DENIAL_EVENT_TYPES, V2_EVENT_TYPE_FS_CALL_DENIED,
    V2_EVENT_TYPE_STEP_DENIED, V2_EVENT_TYPE_TOOL_DENIED,
    V2_INTENTIONALLY_EMPTY_TOOL_WILDCARD_ROOTS, V2_TOOL_WILDCARD_ROOTS, V2AuditEnvelope,
    V2AuditEvent, V2AuditEventKind, check_retired_backend_value, tool_allowed,
    validate_activity_tool_allowlist, validate_activity_tool_allowlist_against_registered_tools,
    validate_job_retired_sessions, validate_tool_allowlist,
    validate_tool_allowlist_against_registered_tools,
};
pub use auto_task::{
    AUTO_TASK_SCHEMA_VERSION, AUTO_TASK_TAG_PREFIX, AutoTaskDefinition, AutoTaskSchedule,
    AutoTaskTemplate, DedupePolicy, MAX_AUTO_TASK_INTERVAL_MINUTES, auto_task_tag,
    is_valid_auto_task_name,
};
pub use child_dispatch::{
    ChildCancellation, ChildCancellationPolicy, ChildDispatch, ChildDispatchPhase,
};
pub use executor_def::{
    ExecutorDef, ExecutorSandboxKind, ExecutorType, ModelPairOverride, StdoutFormat,
};
pub use job::{
    AgentCommitRequest, AgentResponseEnvelope, AgentRunError, Job, JobRun, JobRunStartOutcome,
    JobRunState, JobRunStep, JobScheduleState, JobStep, JobTargetType, KnowledgeRunMetrics,
    RunEvent, RunStateUpdate, StepCondition, default_job_max_active_runs, default_max_iterations,
    default_retry_backoff_seconds,
};
pub use operation::{
    GrantAdmission, GrantLimits, GrantRights, GrantStatus, GrantTransition, MAX_GRANT_SCOPE_TASKS,
    MAX_GRANT_WINDOW_SECONDS, OPERATION_ADMISSION_KEY, OperationAdmission, OperationGrant,
    RecoveryEpisode, RecoveryEpisodeKind, RecoveryLedger, RecoveryReservation,
};
pub use review::{
    CommitIdentity, DEFAULT_REVIEW_MINUTES, DEFAULT_REVIEW_REPAIR_CYCLES,
    DEFAULT_REVIEW_REVIEWER_STARTS, FindingDisposition, LandingTransformation,
    REVIEW_ADMISSION_KEY, REVIEW_CONTRACT_VERSION, REVIEW_GATE_ARTIFACT, REVIEW_MANIFEST_ARTIFACT,
    REVIEW_REPORT_ARTIFACT, ReviewAdmission, ReviewAssurance, ReviewAttempt, ReviewAttemptState,
    ReviewBudget, ReviewCertificate, ReviewConsumption, ReviewFinding, ReviewInvalidation,
    ReviewLanding, ReviewLedger, ReviewManifest, ReviewReport, ReviewReservation, ReviewTiming,
    ReviewValidation, ReviewVerdict, ReviewerIdentity, ValidationOutcome, ValidationRole,
};
pub use routine::{
    MissedRunPolicy, OverlapPolicy, ROUTINE_SCHEMA_VERSION, RoutineDefinition, RoutinePolicy,
    RoutineRetries, RoutineTarget, RoutineTrigger,
};
pub use run_id::{RunIdRole, run_id_candidate, run_id_minute_stem, run_id_role};
pub use run_state::{
    DrainAdmissionsStop, DrainWorkerLimit, FailureActivityCheckpoint, PipelineState,
};
pub use ship::{CompletionPolicy, ShipMode, resolved_ship_mode};
pub use skill::Skill;

mod auto_task_cursor;
pub use auto_task_cursor::{AutoTaskCursor, AutoTaskCursorState, AutoTaskPendingClaim};

pub mod automation;
