//! Trusted observe/finalize integration. The assignment lock fences a cycle
//! change against a comment append and serializes admission with manager
//! control; no agent-facing tool can set this state.

mod lifecycle;

#[cfg(feature = "test-support")]
mod test_support;

pub use lifecycle::set_resident_live_cycle;
pub(super) use lifecycle::{live_cycle_from_history, publish_resident_live_cycle};
#[cfg(feature = "test-support")]
pub use test_support::with_resident_cycle_admission_checkpoint;
