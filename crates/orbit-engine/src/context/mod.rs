//! Execution-context primitives shared by the engine flows.
//!
//! Split by concern; every item is re-exported here so `crate::context::X`
//! paths (and the crate-root re-exports in `lib.rs`) stay stable:
//! - [`outcome`] — run outcome types, error-code constants, and
//!   workflow-failure helpers.
//! - [`hosts`] — the unified [`RuntimeHost`] boundary and task-update types.
//! - [`env`] — subprocess provenance environment variables shared by every
//!   engine spawn path.

mod env;
mod hosts;
mod outcome;

#[cfg(test)]
mod tests;

pub(crate) use env::{ProvenanceEnv, provenance_env};
pub use hosts::{
    CrewConfig, PrConfig, ResolvedActivityTools, ReviewLandingRequest, RuntimeHost,
    StepRecoveryAdmission, TaskActivityUpdate, TaskAutomationUpdate,
};
pub use outcome::{
    AGENT_INVOCATION_FAILED, AGENT_TIMEOUT, ActivityInvocationResult, WORKFLOW_RUN_FAILED_EVENT,
    blocked_workflow_failure_update,
};
