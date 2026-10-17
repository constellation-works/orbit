//! Step failure recovery: bounded inputs, durable decisions, retries and evidence.

mod check_scope;
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
/// The pool step-failure and PR conflict recovery draw their crew from: the
/// failure-recovery purpose pool. While it is unset the host draws from the
/// medium complexity pool [ORB-15663]: recovery diagnoses a failure in work of
/// any tier, so it is not low work, and the medium pool is the one an
/// unassessed task is verified on.
const RECOVERY_CREWS_KEY: &str = "workflow.purpose_crews.failure_recovery";

/// The job's `step_failure_recovery`, resolved from the first top-level step
/// that declares it: the repair an `--auto-fix` check failure is routed to.
/// A job none of whose steps declares it routes no check repair.
pub(super) fn check_repair_activity(job: &JobV2) -> Option<ResolvedRecoveryActivity> {
    job.steps.iter().find_map(|step| {
        match (
            step.recovery_activity.as_deref(),
            step.resolved_recovery_activity.as_ref(),
        ) {
            (Some(STEP_FAILURE_RECOVERY_ACTIVITY), Some(activity)) => {
                Some(ResolvedRecoveryActivity {
                    name: STEP_FAILURE_RECOVERY_ACTIVITY.to_string(),
                    spec: activity.spec.clone(),
                })
            }
            _ => None,
        }
    })
}
