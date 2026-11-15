//! Trusted observe/finalize integration. The assignment lock fences a cycle
//! change against a comment append and serializes admission with manager
//! control; no agent-facing tool can set this state.

#[cfg(feature = "test-support")]
pub(crate) mod test_support;

mod lifecycle;
pub(super) use lifecycle::{CYCLE_CHANGED, live_cycle_from_history};
