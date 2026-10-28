//! [ORB-13907] The job-level final recovery hook.
//!
//! When a top-level step fails after its own retry and step recovery are
//! spent — or skipped, for error kinds that bypass step recovery — a job that
//! declares `final_recovery_activity` gets one last automated look before its
//! `failure_activity`. The activity returns a typed
//! [`orbit_types::workflow::FinalRecoveryDecision`]; the engine owns `resume`, and the host applies
//! every other decision through the deterministic applier.
//!
//! The hook runs at most once per run. The host records its admission in run
//! state before dispatch, and the executor also refuses a second invocation
//! in the same process and in any run resumed from that state. A run resumed
//! from state that already recorded a decision other than `resume` hands that
//! decision to the host again instead of dispatching. The host applies a
//! decision idempotently, so a crash anywhere in the first application
//! converges on the same outcome.
//!
//! A `sync_base` failure that reports an absorbed candidate (conflict
//! recovery left the branch on its pinned base, which already carries the
//! change) is admitted the same way, but the engine re-verifies the certified
//! evidence and hands the host a `complete_no_diff` for the covering commit
//! instead of dispatching the activity.

mod attempt;
mod decision;
mod input;
mod settlement;
mod types;

pub(super) use attempt::attempt_final_recovery;
pub(super) use types::{FinalRecoveryBudget, FinalRecoveryVerdict, RunWorktree};
