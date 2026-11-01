//! The deterministic applier for `final_recovery` decisions.
//!
//! The final-recovery agent only proposes. Every lifecycle write a decision
//! implies happens here, under the task write lock, after re-checking the
//! task against the revision observed when the run failed. The engine hook
//! and the out-of-pipeline backstop both apply decisions through this one
//! module, so a decision means the same thing whichever path produced it.

mod apply;
mod mechanical;
mod request;
mod revision;
mod verify;

pub use crate::runtime::task::FINAL_RECOVERY_REQUEUED_EVENT;
pub(in crate::application::task) use mechanical::{operator_only_requeue, records_base_red};
pub(crate) use request::TERMINAL_REFUSAL_MARK;

/// History event a `resume_run` step appends; the requeue bound counts it
/// with requeues, and its note names the resumed run so a run resumes once.
pub const FINAL_RECOVERY_RESUMED_EVENT: &str = "final_recovery_resumed";
/// History event a `rerun_ci` step appends; its note names the head whose
/// failed checks were re-run so a head is re-run once.
pub const FINAL_RECOVERY_CI_RERUN_EVENT: &str = "final_recovery_ci_rerun";
pub use request::{
    FinalRecoveryCompletion, FinalRecoveryOutcome, FinalRecoveryRequest, FinalRecoveryRequeueBound,
};
pub use revision::FinalRecoveryTaskRevision;

#[cfg(test)]
mod tests;
