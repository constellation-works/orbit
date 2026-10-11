//! Step failure recovery: bounded inputs, durable decisions, retries and evidence.

mod commit;
mod decision;
pub(super) mod evidence;
mod failure;
mod input;
mod invocation;
mod observation;
mod retry;

use super::*;

pub(super) use failure::attempt_failure_activity;
pub(super) use input::{
    MAX_RECOVERY_ERROR_MESSAGE_BYTES, bounded_recovery_input, bounded_recovery_text,
    redacted_recovery_diagnostic,
};
pub(super) use invocation::crew_overridden_recovery_spec;
pub(super) use retry::{StepFailure, recover_or_return_original};

const PR_CONFLICT_RECOVERY_ACTIVITY: &str = "pr_conflict_recovery";
const STEP_FAILURE_RECOVERY_ACTIVITY: &str = "step_failure_recovery";
