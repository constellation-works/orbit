//! Held delivery leaves an out-of-run pass hands back [ORB-23809].
//!
//! A delivery leaf that ends `held` with a typed delivery refusal leaves its
//! task in `review` and its pull request open. For the refusals a fresh forge
//! read can clear ([`handed_back_hold`]), an `--auto-fix` drain pass (or the
//! dashboard's Fix PR) hands the task back to the leaf's completion step, the
//! same resume a failed, timed-out or interrupted leaf gets. Every other hold
//! keeps its own documented next step.
//!
//! The hand-backs of one held lineage are bounded without the pull request's
//! repair ledger, which they never spend:
//!
//! - **backoff**: a drain pass hands a held leaf back only
//!   [`held_backoff`] after the leaf ended: [`HELD_BACKOFF_BASE`], doubled for
//!   each earlier hand-back of the same pull request for the same hold code;
//! - **ceiling**: at most [`HELD_HANDBACK_CEILING`] hand-backs per pull
//!   request, head and hold code. A lineage that keeps holding for the same
//!   reason on the same head is then left to an operator, and the pass
//!   reports it under the hold code.

use std::time::Duration;

use chrono::{DateTime, Utc};
use orbit_types::task::TaskHistoryEntry;
use orbit_types::workflow::{DeliveryRefusal, JobRun, JobRunState};

use super::ledger::out_of_run_starts;

/// Hand-backs of one pull request, head and hold code before the pass leaves
/// the held lineage to an operator.
pub(super) const HELD_HANDBACK_CEILING: usize = 3;

/// The wait after a held leaf ended before its first hand-back; each earlier
/// hand-back of the same pull request for the same hold code doubles it.
pub(super) const HELD_BACKOFF_BASE: Duration = Duration::from_secs(300);

/// The delivery refusals a fresh forge read can clear, so completion is
/// handed the pull request again. Each other refusal names its own next step
/// (an operator decision, a new head or a new delivery attempt), the clock or
/// orphan adoption resumes it, or it carries no pull request; the decision
/// per code is recorded in the out-of-run auto-fix design's decisions.
pub(crate) fn handed_back_hold(refusal: DeliveryRefusal) -> bool {
    match refusal {
        DeliveryRefusal::CheckEvidenceInconclusive
        | DeliveryRefusal::CompletionBudgetExpired
        | DeliveryRefusal::EvidenceStale
        | DeliveryRefusal::RequirementsUnsatisfied
        | DeliveryRefusal::BaseChecksPending
        | DeliveryRefusal::ForgeObservationUnavailable => true,
        DeliveryRefusal::HeadChanged
        | DeliveryRefusal::RepairDispatched
        | DeliveryRefusal::PullRequestDraft
        | DeliveryRefusal::PullRequestClosed
        | DeliveryRefusal::TaskNotPromotable
        | DeliveryRefusal::LandingConflict
        | DeliveryRefusal::BaseChaseExhausted
        | DeliveryRefusal::OriginUnavailable
        | DeliveryRefusal::CheckRepairActivityAbsent
        | DeliveryRefusal::VisualNotEstablished
        | DeliveryRefusal::VisualReviewUnresolved => false,
    }
}

/// The typed delivery refusal `run` ended `held` with, read from its last
/// step's diagnostic code; `None` for a run that is not held or whose hold
/// is not a delivery refusal. `run` must carry its steps.
pub(super) fn held_refusal(run: &JobRun) -> Option<DeliveryRefusal> {
    if run.state != JobRunState::Held {
        return None;
    }
    run.steps
        .last()
        .and_then(|step| step.error_code.as_deref())
        .and_then(DeliveryRefusal::from_error_code)
}

/// How long after a held leaf ended the next hand-back for `code` waits,
/// given the pull request's history.
pub(super) fn held_backoff(
    history: &[TaskHistoryEntry],
    pr_number: &str,
    code: DeliveryRefusal,
) -> Duration {
    let earlier = out_of_run_starts(history, pr_number)
        .iter()
        .filter(|start| start.hold_code.as_deref() == Some(code.error_code()))
        .count()
        .min(HELD_HANDBACK_CEILING);
    HELD_BACKOFF_BASE.saturating_mul(1 << earlier)
}

/// When a held leaf that ended at `finished_at` may next be handed back, or
/// `None` when it may be now.
pub(super) fn held_backoff_until(
    finished_at: Option<DateTime<Utc>>,
    backoff: Duration,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    let until = finished_at? + chrono::Duration::from_std(backoff).ok()?;
    (until > now).then_some(until)
}

/// How many times the pull request was handed back on `head_sha` for `code`.
pub(super) fn held_handbacks(
    history: &[TaskHistoryEntry],
    pr_number: &str,
    head_sha: &str,
    code: DeliveryRefusal,
) -> usize {
    out_of_run_starts(history, pr_number)
        .iter()
        .filter(|start| {
            start.head_sha == head_sha && start.hold_code.as_deref() == Some(code.error_code())
        })
        .count()
}
