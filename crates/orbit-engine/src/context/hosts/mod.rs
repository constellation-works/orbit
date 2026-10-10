//! The host trait boundary between the engine and its runtime/store
//! implementors, plus the task-update param types those traits consume.

mod claim;
mod config;
mod plugin_broker;
mod recovery;
mod review;
mod runtime_host;
mod scratch_gc;
mod task_update;
mod worktree_gc;

pub use claim::{
    ClaimExecutionContext, HandoffLandingContext, HandoffLandingStep, HandoffLandingUpdate,
};
pub use config::{CrewConfig, PrConfig};
pub use plugin_broker::{PLUGIN_BROKER_ENV, PluginBrokerHandle, PluginBrokerRun};
pub use recovery::{
    FinalRecoveryAdmission, FinalRecoveryAdmissionRequest, FinalRecoveryApplication,
    FinalRecoveryApplied, RebaseRecoveryAttemptScope, STEP_RECOVERY_DECISION_SCHEMA_VERSION,
    StepRecoveryAdmission, StepRecoveryDecisionRead, StepRecoveryDecisionRequest,
    StepRecoveryDecisionSlot, StepRecoveryVerdict,
};
pub use review::{
    ReviewLandingRequest, ReviewReleaseRequest, ReviewReportCorrectionRequest,
    ReviewerInvocationRequest,
};
pub use runtime_host::RuntimeHost;
pub use scratch_gc::{ScratchGcEntry, ScratchGcReport};
pub use task_update::{ResolvedActivityTools, TaskActivityUpdate, TaskAutomationUpdate};
pub use worktree_gc::WorktreeGcTaskLookup;
