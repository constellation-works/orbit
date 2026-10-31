//! Readers of a run's audit trail: the trail accessor and its shared types,
//! the bounded run-detail projection, the run-events query, the CLI-invocation
//! log profiles, script-step attempts, and the pure projections they share.

pub(crate) mod detail;
pub mod events;
pub mod logs;
mod projection;
mod scripts;
mod trail;

pub use scripts::RunScriptExecution;

pub use trail::{
    MAX_RECOVERY_ATTEMPTS, RECOVERY_FETCH_PER_RUN, RunAuditEvent, RunAuditStep, RunAuditView,
    RunCliInvocationRecord, RunExecutionProgress, RunProviderProcess, RunRecoveryAttempt,
    RunRecoveryAttempts, RunRecoveryAttemptsPage, RunRecoveryDecision, RunStoppedDescendant,
};
