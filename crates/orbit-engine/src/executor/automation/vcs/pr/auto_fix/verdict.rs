use orbit_common::OrbitError;
use orbit_types::workflow::{DeliveryRefusal, PR_AUTO_FIX_CHECK_REPAIR_MARKER, PrAutoFixAttempt};

use super::super::answers::{CheckState, PullRequestStatus};
use super::super::merge_state::{BlockedCheckRefusal, blocked_checks};

/// Polls a `BLOCKED` head with no failed and no running check is waited on
/// before it counts as blocked: a freshly published head can report no
/// checks until the provider queues them.
pub(in crate::executor::automation::vcs::pr) const UNSETTLED_BLOCKED_POLLS: u32 = 4;

/// A gate refusal forbids infrastructure recovery as well as source repair;
/// unsatisfied checks alone may still allow bounded infrastructure recovery.
#[derive(Debug, thiserror::Error)]
pub(in crate::executor::automation::vcs::pr) enum BlockedStopReason {
    #[error(transparent)]
    CheckRefusal(BlockedCheckRefusal),
    #[error("required reviews or checks are not satisfied")]
    UnsatisfiedChecks,
}

/// What a `BLOCKED` pull request means to an auto-fix watcher.
pub(in crate::executor::automation::vcs::pr) enum BlockedVerdict {
    /// Checks are still running, or the head has not settled yet.
    Wait,
    /// Required checks failed: repair them.
    RepairChecks(Vec<(Option<String>, String)>),
    /// An administrative blocker no code repair clears.
    Stop(BlockedStopReason),
}

/// Read a `BLOCKED` pull request for auto-fix. A review gate or an
/// unreadable rollup stops before any check is considered, as completion
/// already does; only policy-bound pending evidence authorizes a check wait.
pub(in crate::executor::automation::vcs::pr) fn blocked_verdict(
    status: &PullRequestStatus,
    unsettled_polls: u32,
    pending_required: bool,
) -> BlockedVerdict {
    let checks = match blocked_checks(status) {
        Ok(checks) => checks,
        Err(reason) => return BlockedVerdict::Stop(BlockedStopReason::CheckRefusal(reason)),
    };
    if pending_required {
        return BlockedVerdict::Wait;
    }
    let failed = checks
        .iter()
        .filter(|check| check.state == Some(CheckState::Failed))
        .filter_map(|check| Some((check.workflow_name.clone(), check.name.clone()?)))
        .collect::<Vec<_>>();
    if !failed.is_empty() {
        return BlockedVerdict::RepairChecks(failed);
    }
    if unsettled_polls < UNSETTLED_BLOCKED_POLLS {
        return BlockedVerdict::Wait;
    }
    BlockedVerdict::Stop(BlockedStopReason::UnsatisfiedChecks)
}

/// The failure that stops an auto-fix watcher without an agent.
pub(in crate::executor::automation::vcs::pr) fn stopped(
    pr_number: &str,
    reason: &str,
) -> OrbitError {
    OrbitError::Execution(format!(
        "auto_fix_stopped: pull request #{pr_number} cannot be repaired automatically \
         ({reason}); no repair agent runs, the pull request stays open and the task stays in \
         review"
    ))
}

/// Give a deliberate stop its typed hold while preserving its diagnostic.
pub(in crate::executor::automation::vcs::pr) fn hold_auto_fix_stop(
    error: OrbitError,
    refusal: DeliveryRefusal,
) -> OrbitError {
    match error {
        OrbitError::Execution(message) => OrbitError::DeliveryRefused { refusal, message },
        other => other,
    }
}

/// [`stopped`] on a provider read that failed with `cause`, keeping the class
/// of the private VCS failure it reports.
pub(in crate::executor::automation::vcs::pr) fn stopped_by(
    pr_number: &str,
    reason: &str,
    cause: &OrbitError,
) -> OrbitError {
    super::super::super::operations::carry_class(cause, stopped(pr_number, reason))
}

/// The failure that hands an admitted check repair to the step's repair
/// activity, with the head-bound evidence it repairs from.
pub(in crate::executor::automation::vcs::pr) fn check_repair_handoff(
    pr_number: &str,
    head_sha: &str,
    attempt: &PrAutoFixAttempt,
    evidence: &str,
) -> OrbitError {
    OrbitError::Execution(format!(
        "{PR_AUTO_FIX_CHECK_REPAIR_MARKER}: pull request #{pr_number} failed required checks on \
         head {head_sha}; auto-fix repair attempt {} of {} runs as crew '{}' (drawn from {}). \
         Repair the candidate in the assigned worktree so these checks pass and leave the \
         repair uncommitted: Orbit commits it, reruns required validation, publishes it on the \
         pull request and watches the new head.\n{evidence}",
        attempt.attempt, attempt.budget, attempt.crew, attempt.crew_source
    ))
}
