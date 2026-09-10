//! Activity/job runtime types.

/// `activity:<name>` prefix for the `target:` field on a TargetRef.
pub const ACTIVITY_REF_PREFIX: &str = "activity:";

pub mod activity_roles;
pub mod activity_v2;
pub mod audit_envelope;
pub mod job_v2;
pub mod retired;
pub mod schema_header;
pub mod tool_allowlist;
pub mod trusted_host;

/// The single declaration of the deterministic action catalog. The generated
/// typed actions make the core/engine ownership boundary exhaustive at compile
/// time while keeping action names available to YAML assets at runtime.
macro_rules! deterministic_action_catalog {
    ($declare:ident) => {
        $declare! {
            core {
                ApplyTriageDispositions => "apply_triage_dispositions",
                ApplyTaskPilotResults => "apply_task_pilot_results",
                ClassifyWorkspaceAutoTasks => "classify_workspace_auto_tasks",
                ConsolidateCodeScanningTasks => "consolidate_code_scanning_tasks",
                ContextConflictCheck => "context_conflict_check",
                DrainWindow => "drain_window",
                FileCiFailureTasks => "file_ci_failure_tasks",
                FileDependabotAlertTasks => "file_dependabot_alert_tasks",
                GateStarvationFail => "gate_starvation_fail",
                InvokeAndWait => "invoke_and_wait",
                InvokeDetached => "invoke_detached",
                ListBacklogTasks => "list_backlog_tasks",
                ListEpicDescendants => "list_epic_descendants",
                ListTriageCandidates => "list_triage_candidates",
                OrbitToolCall => "orbit_tool_call",
                PipelineSuccessGuard => "pipeline_success_guard",
                PrepareTaskPilot => "prepare_task_pilot",
                PromoteAgentMain => "promote_agent_main",
                ReleaseLocks => "release_locks",
                ReserveLocks => "reserve_locks",
                ResolveWorkspaceShipInput => "resolve_workspace_ship_input",
                ReviewGateAdmit => "review_gate_admit",
                ReviewGateSettle => "review_gate_settle",
                RevertOnRed => "revert_on_red",
                RunAutoTaskScheduler => "run_auto_task_scheduler",
                ScanUnresolvedWork => "scan_unresolved_work",
                Sleep => "sleep",
                ValidateBundles => "validate_bundles",
            }
            engine {
                CollectCiEvidence => "collect_ci_evidence",
                CollectDependabotAlerts => "collect_dependabot_alerts",
                GitCommit => "git_commit",
                GitMerge => "git_merge",
                GitPush => "git_push",
                GitRebase => "git_rebase",
                LocalShell => "local_shell",
                PrFailureHandoff => "pr_failure_handoff",
                PrOpen => "pr_open",
                PrPrepare => "pr_prepare",
                PrComplete => "pr_complete",
                PrPromote => "pr_promote",
                TaskComplete => "task_complete",
                UpdateTask => "update_task",
                WorktreeGc => "worktree_gc",
                WorktreeSetup => "worktree_setup",
            }
        }
    };
}

macro_rules! define_deterministic_actions {
    (
        core { $( $core_variant:ident => $core_name:literal, )* }
        engine { $( $engine_variant:ident => $engine_name:literal, )* }
    ) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum CoreDeterministicAction {
            $( $core_variant, )*
        }

        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum EngineDeterministicAction {
            $( $engine_variant, )*
        }

        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum DeterministicAction {
            Core(CoreDeterministicAction),
            Engine(EngineDeterministicAction),
        }

        impl CoreDeterministicAction {
            pub const fn name(self) -> &'static str {
                match self {
                    $( Self::$core_variant => $core_name, )*
                }
            }
        }

        impl EngineDeterministicAction {
            pub const fn name(self) -> &'static str {
                match self {
                    $( Self::$engine_variant => $engine_name, )*
                }
            }
        }

        impl DeterministicAction {
            pub const NAMES: &[&str] = &[
                $( $core_name, )*
                $( $engine_name, )*
            ];

            pub fn parse(name: &str) -> Option<Self> {
                match name {
                    $( $core_name => Some(Self::Core(CoreDeterministicAction::$core_variant)), )*
                    $( $engine_name => Some(Self::Engine(EngineDeterministicAction::$engine_variant)), )*
                    _ => None,
                }
            }

            pub const fn name(self) -> &'static str {
                match self {
                    Self::Core(action) => action.name(),
                    Self::Engine(action) => action.name(),
                }
            }
        }
    };
}

deterministic_action_catalog!(define_deterministic_actions);

pub use activity_roles::JobActivityRoles;
pub use activity_v2::{
    ActivityV2, ActivityV2Spec, AgentLoopSpec, DeterministicSpec, OnDenial, Provider,
    ProviderAlias, ProviderDeprecation, ProviderDiagnostic, ProviderEntryPoint, ProviderIdentity,
    ProviderParseError, ProviderResolution, ProviderResolveRequest, ProviderSource,
    RETIRED_BACKEND_MIGRATION, RetiredAgentBackend, check_retired_backend_value,
};
pub use audit_envelope::{
    AUDIT_ENVELOPE_SCHEMA_VERSION, BranchOutcome, V2_DENIAL_EVENT_TYPES,
    V2_EVENT_TYPE_FS_CALL_DENIED, V2_EVENT_TYPE_STEP_DENIED, V2_EVENT_TYPE_TOOL_DENIED,
    V2AuditEnvelope, V2AuditEvent, V2AuditEventKind,
};
pub use job_v2::{
    BackoffStrategy, FanInSpec, FanOutBlock, JobKind, JobV2, JobV2Step, JobV2StepBody, JoinMode,
    LoopBlock, ParallelBlock, PipelineRef, RetrySpec, TargetRef, TargetStep,
};
pub use retired::{RetiredFeatureError, validate_job_retired_sessions};
pub use schema_header::SchemaHeader;
pub use tool_allowlist::{
    ToolAllowlistError, V2_INTENTIONALLY_EMPTY_TOOL_WILDCARD_ROOTS, V2_TOOL_WILDCARD_ROOTS,
    tool_allowed, validate_activity_tool_allowlist,
    validate_activity_tool_allowlist_against_registered_tools, validate_tool_allowlist,
    validate_tool_allowlist_against_registered_tools,
};
pub use trusted_host::{
    TRUSTED_HOST_ACTIVITY, TRUSTED_HOST_ADMISSION_KEY, TrustedHostActivityError,
    TrustedHostAdmission, run_input_declares_trusted_host, strip_trusted_host_admission,
    validate_trusted_host_activity,
};

#[cfg(test)]
mod tests;
