//! The deterministic applier for `final_recovery` decisions.
//!
//! The final-recovery agent only proposes. Every lifecycle write a decision
//! implies happens here, under the task write lock, after re-checking the
//! task against the revision observed when the run failed. The engine hook
//! and the out-of-pipeline backstop both apply decisions through this one
//! module, so a decision means the same thing whichever path produced it.

mod apply;
mod request;
mod revision;
mod verify;

pub use crate::runtime::task::FINAL_RECOVERY_REQUEUED_EVENT;
pub use request::{
    FinalRecoveryCompletion, FinalRecoveryOutcome, FinalRecoveryRequest, FinalRecoveryRequeueBound,
};
pub use revision::FinalRecoveryTaskRevision;

#[cfg(test)]
mod tests;
