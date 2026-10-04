//! Execution-context primitives shared by the engine flows.
//!
//! Split by concern; every item is re-exported here so `crate::context::X`
//! paths (and the crate-root re-exports in `lib.rs`) stay stable:
//! - [`outcome`] — run outcome types, error-code constants, and
//!   workflow-failure/interruption helpers.
//! - [`hosts`] — the unified [`RuntimeHost`] boundary and task-update types.
//! - [`env`] — subprocess provenance environment variables shared by every
//!   engine spawn path.

mod env;
mod hosts;
mod outcome;

pub(crate) use env::{ProvenanceEnv, provenance_env};
pub use hosts::{
    ClaimExecutionContext, CrewConfig, FinalRecoveryAdmission, FinalRecoveryAdmissionRequest,
    FinalRecoveryApplication, FinalRecoveryApplied, HandoffLandingContext, HandoffLandingStep,
    HandoffLandingUpdate, PLUGIN_BROKER_ENV, PluginBrokerHandle, PluginBrokerRun, PrConfig,
    ResolvedActivityTools, ReviewLandingRequest, ReviewReleaseRequest, ReviewerInvocationRequest,
    RuntimeHost, StepRecoveryAdmission, TaskActivityUpdate, TaskAutomationUpdate,
    WorktreeGcTaskLookup,
};
pub use outcome::{
    WORKFLOW_RUN_FAILED_EVENT, WORKFLOW_RUN_INTERRUPTED_EVENT, blocked_workflow_failure_update,
    blocked_workflow_interruption_update,
};
