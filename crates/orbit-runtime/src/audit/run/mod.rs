//! Readers of a run's audit trail: the trail accessor and its shared types,
//! the bounded run-detail projection, the run-events query, the CLI-invocation
//! log profiles, and the pure projections they share.

pub(crate) mod detail;
pub mod events;
pub mod logs;
mod projection;
mod trail;

pub use trail::{
    MAX_RECOVERY_ATTEMPTS, RECOVERY_FETCH_PER_RUN, RunAuditEvent, RunAuditStep, RunAuditView,
    RunCliInvocationRecord, RunExecutionProgress, RunProviderProcess, RunRecoveryAttempt,
    RunRecoveryAttempts, RunRecoveryAttemptsPage, RunRecoveryDecision, RunStoppedDescendant,
};
