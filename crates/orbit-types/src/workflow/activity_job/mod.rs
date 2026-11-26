//! Activity/job runtime types.

pub mod activity_roles;
pub mod activity_v2;
pub mod audit_envelope;
mod deterministic_actions;
pub mod job_v2;
pub mod provider_sandbox;
pub mod response_correction;
pub mod schema_header;
pub mod script;
pub mod tool_allowlist;
pub mod trusted_host;
mod worktree_failure;

pub use activity_roles::JobActivityRoles;
pub use activity_v2::{
    ActivityV2, ActivityV2Spec, AgentLoopSpec, DeterministicSpec, FleetNoticeGuidance,
    IncompleteContinuationPolicy, OnDenial, Provider, ProviderAlias, ProviderDeprecation,
    ProviderIdentity, ProviderParseError, ProviderSource, RETIRED_BACKEND_MIGRATION,
    RETIRED_PROVIDER_IDS, RETIRED_PROVIDER_MIGRATION, ResponseCorrectionPolicy,
    RetiredAgentBackend, check_retired_backend_value, requires_native_inspection,
    retired_provider_id,
};
pub use audit_envelope::{
    AUDIT_ENVELOPE_SCHEMA_VERSION, BranchOutcome, StepRecoveryDecisionRecord,
    V2_DENIAL_EVENT_TYPES, V2_EVENT_TYPE_FS_CALL_DENIED, V2_EVENT_TYPE_SCRIPT_EXECUTION,
    V2_EVENT_TYPE_STEP_DENIED, V2_EVENT_TYPE_TOOL_DENIED, V2AuditEnvelope, V2AuditEvent,
    V2AuditEventKind,
};
pub use deterministic_actions::{
    CoreDeterministicAction, DeterministicAction, EngineDeterministicAction,
};
pub use job_v2::{
    BackoffStrategy, FanInSpec, FanOutBlock, JobKind, JobStage, JobTaskDelivery, JobV2, JobV2Step,
    JobV2StepBody, JoinMode, LoopBlock, ParallelBlock, PipelineRef, RetrySpec, SteeringGateSpec,
    TargetRef, TargetStep,
};
pub use provider_sandbox::CODEX_PROVIDER_SANDBOX_MODES;
pub use response_correction::{
    MUTATING_TOOLS, ResponseCorrectionActivityError, is_response_correction_mutating_tool,
    validate_response_correction_activity,
};
pub use schema_header::SchemaHeader;
pub use script::{
    SCRIPT_GRANT_DIFF_MAX_BYTES, SCRIPT_GRANT_TASK_TAG, ScriptGrantCapabilities,
    ScriptGrantRequest, script_file_diff, script_grant_diff,
};
pub use script::{
    SCRIPT_GRANT_DIGEST_DOMAIN, SCRIPT_INPUT_MAX_TREE_ENTRIES, SCRIPT_INPUT_TREE_DOMAIN,
    SCRIPT_MANIFEST_SCHEMA_VERSION, SCRIPT_OUTPUT_HARD_MAX_FILE_BYTES,
    SCRIPT_OUTPUT_HARD_MAX_FILES, SCRIPT_OUTPUT_HARD_MAX_TOTAL_BYTES, SCRIPT_OUTPUT_MAX_NAME_BYTES,
    SCRIPT_OUTPUT_SET_DOMAIN, SCRIPT_PROFILE_LINUX, SCRIPT_PROFILE_MACOS, SCRIPT_STDOUT_CAP_BYTES,
    ScriptExecutionRecord, ScriptFileInput, ScriptFileOutputs, ScriptFiles, ScriptGrant,
    ScriptGrantLimits, ScriptHostRole, ScriptInputFile, ScriptNetwork, ScriptOutputFile,
    ScriptOutputSet, ScriptRequests, ScriptSpec, ScriptValidationError, is_valid_script_name,
    is_valid_script_output_name, script_content_digest, script_failure, script_grant_id,
    script_interpreter_from_shebang, script_output_media_type, script_output_set_id,
    script_output_total_cap, script_refusal, script_sha256_hex, validate_script_arg_template,
    validate_script_interpreter, validate_script_spec,
};
pub use tool_allowlist::{
    ActivityToolDenyPolicy, ActivityToolPolicyMode, ToolAllowlistError,
    V2_INTENTIONALLY_EMPTY_TOOL_WILDCARD_ROOTS, V2_TOOL_WILDCARD_ROOTS,
    activity_tool_policy_deprecation, tool_allowed, tools_allowed_by_disallow_list,
    validate_activity_tool_allowlist, validate_activity_tool_allowlist_against_registered_tools,
    validate_tool_allowlist, validate_tool_allowlist_against_registered_tools,
};
pub use trusted_host::{
    TRUSTED_HOST_ADMISSION_KEY, TrustedHostActivityError, TrustedHostAdmission,
    run_input_declares_trusted_host, strip_trusted_host_admission, validate_trusted_host_activity,
};
pub use worktree_failure::{legacy_worktree_failure, worktree_failure_message};

/// `activity:<name>` prefix for the `target:` field on a TargetRef.
pub const ACTIVITY_REF_PREFIX: &str = "activity:";

/// `script:<name>` prefix for the `target:` field on a TargetRef.
pub const SCRIPT_REF_PREFIX: &str = script::SCRIPT_REF_PREFIX;

/// `auto-task:<name>` prefix for the `target:` field on a TargetRef: a step
/// that binds an auto-task and waits for its accepted completion
/// (`docs/design/activity-job/specs/auto-task-step.md`). Such steps can be
/// defined and explained; no binary runs them yet.
pub const AUTO_TASK_REF_PREFIX: &str = "auto-task:";

/// The `schemaVersion` every activity and every job without the auto-task
/// step extension declares.
pub const ASSET_SCHEMA_VERSION: u32 = 2;

/// The `schemaVersion` a job using the auto-task step extension must declare,
/// and no activity may: a binary that predates the extension refuses the
/// whole file as an unsupported version instead of dropping the fields it
/// does not know ([ORB-20237]). A job keeps the version its file declares,
/// whether or not any of the extension's fields remain.
pub const AUTO_TASK_JOB_SCHEMA_VERSION: u32 = 3;
