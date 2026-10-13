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

pub(super) use commit::commit_final_recovery_repair;
pub(super) use failure::attempt_failure_activity;
pub(super) use input::{
    MAX_RECOVERY_ERROR_MESSAGE_BYTES, bounded_recovery_input, bounded_recovery_text,
    redacted_recovery_diagnostic,
};
pub(super) use invocation::crew_overridden_recovery_spec;
pub(super) use retry::{StepFailure, recover_or_return_original};

const PR_CONFLICT_RECOVERY_ACTIVITY: &str = "pr_conflict_recovery";
const STEP_FAILURE_RECOVERY_ACTIVITY: &str = "step_failure_recovery";
/// The pool step-failure and PR conflict recovery draw their crew from
/// [ORB-15663]. Medium: recovery diagnoses a failure in work of any tier, so
/// it is not low work, and the medium pool is the one an unassessed task is
/// verified on.
const RECOVERY_CREWS_KEY: &str = "workflow.medium_complexity_crews";
