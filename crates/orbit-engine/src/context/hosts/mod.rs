//! The host trait boundary between the engine and its runtime/store
//! implementors, plus the task-update param types those traits consume.

mod auto_task;
mod claim;
mod config;
#[cfg(feature = "test-support")]
mod null_host;
mod plugin_broker;
mod recovery;
mod revalidation;
mod review;
mod runtime_host;
mod scratch_gc;
mod script_step;
mod steering;
mod task_update;
mod worktree_gc;

pub use auto_task::{
    AutoTaskAdmission, AutoTaskObservation, AutoTaskReadiness, AutoTaskStepRequest,
};
pub use claim::{
    ClaimExecutionContext, HandoffLandingContext, HandoffLandingStep, HandoffLandingUpdate,
};
pub use config::{CrewConfig, PrConfig};
#[cfg(feature = "test-support")]
pub use null_host::NullRuntimeHost;
pub use plugin_broker::{PLUGIN_BROKER_ENV, PluginBrokerHandle, PluginBrokerRun};
pub use recovery::{
    CompletionObservationAdmission, FinalRecoveryAdmission, FinalRecoveryAdmissionRequest,
    FinalRecoveryApplication, FinalRecoveryApplied, PrAutoFixAdmission, PrAutoFixRequest,
    RebaseRecoveryAttemptScope, STEP_RECOVERY_DECISION_SCHEMA_VERSION, StepRecoveryAdmission,
    StepRecoveryDecisionRead, StepRecoveryDecisionRequest, StepRecoveryDecisionSlot,
    StepRecoveryVerdict,
};
pub use revalidation::{GeneratedPath, UnlockedPathValidation, ValidatedTree};
pub use review::{
    ReviewLandingRequest, ReviewReleaseRequest, ReviewReportCorrectionRequest,
    ReviewerInvocationRequest,
};
pub use runtime_host::RuntimeHost;
pub use scratch_gc::{ScratchGcEntry, ScratchGcReport};
pub use script_step::{
    ScriptStepEnd, ScriptStepOutputs, ScriptStepRan, ScriptStepRequest, ScriptStepRun,
};
pub use steering::{SteeringGateReading, SteeringRoundAdmission};
pub use task_update::{ResolvedActivityTools, TaskActivityUpdate, TaskAutomationUpdate};
pub use worktree_gc::WorktreeGcTaskLookup;

/// Minutes a provider process may sit idle before its step fails as stalled.
///
/// The former `execution.agent_stall_minutes` default, fixed when the key was removed [ORB-22808].
pub const AGENT_STALL_MINUTES: u64 = 30;

/// Hours a task must have been parked in `someday` before the scheduled
/// worktree sweep may remove its run's worktree.
///
/// The former `worktree.parked_reclaim_after_hours` default, fixed when the key was removed [ORB-22808].
pub const WORKTREE_PARKED_RECLAIM_AFTER_HOURS: u64 = 336;
