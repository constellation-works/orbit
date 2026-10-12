//! The reports a settle-only pass and a pull drain's run pages print: one
//! entry per carried admission, refused settlements, pending deliveries, a
//! leaf's owner claim and a drain's claimed leaves.

use chrono::{DateTime, Utc};
use orbit_store::contracts::{LocalPullAdmission, LocalPullPhase};
use orbit_types::workflow::ClaimFailureClass;
use serde::Serialize;

/// One admission carried by a settle-only pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PullSettlementEntry {
    /// The owner's host-qualified selector.
    pub owner: String,
    /// The drain run that admitted the claim.
    pub drain_run_id: String,
    pub request_id: String,
    pub task_id: Option<String>,
    pub leaf_run_id: Option<String>,
    /// Where the admission stands after the pass:
    ///
    /// - `settled` — the owner accepted its handoff, failure or release;
    /// - `closed_obsolete` — the owner had already ended the claim, so the
    ///   settlement was closed locally (ORB-13639);
    /// - `leaf_running` — its leaf is live and settles itself when it ends;
    /// - `pending_delivery` — a settlement is recorded but did not reach the
    ///   owner; any later pass retries it;
    /// - `settlement_refused` — the owner refused the recorded settlement while
    ///   it still holds the claim; delivery backs off until the owner accepts
    ///   it ([ORB-13979]);
    /// - `release_held` — a forced cancel released the claim but could not
    ///   confirm its leaf stopped; the owner keeps the claim until it does;
    /// - `owner_unreachable` — skipped after an earlier delivery to the same
    ///   owner failed in this pass;
    /// - `awaiting_drain` — not launched yet, and a live drain for its owner
    ///   will carry it;
    /// - `unanswered_request` — no live drain will carry it and the owner
    ///   holds no receipt for the request, so nothing is held on the owner;
    /// - `launch_uncertain` — a launch was never acknowledged; deliberate
    ///   recovery is required;
    /// - `idle` / `refused` — an unanswered request the owner answered with
    ///   nothing ready, or refused, so no claim exists;
    /// - `pending` — stopped by the error in `detail`; any later pass retries;
    /// - `no_owner_route` — this runtime has no federated route to the owner.
    pub outcome: String,
    /// The error that stopped the admission, when one did.
    pub detail: Option<String>,
}

impl PullSettlementEntry {
    /// One line for a terminal report.
    ///
    /// An outcome token alone does not tell an operator whether anything is
    /// left to do, so a line with no recorded error carries what the outcome
    /// means and the next step. `launch_uncertain` always carries it: it is
    /// the one outcome that must not be answered with a second attempt.
    #[must_use]
    pub fn describe(&self) -> String {
        let subject = match (&self.task_id, &self.leaf_run_id) {
            (Some(task), Some(leaf)) => format!("{task} (leaf {leaf})"),
            (Some(task), None) => task.clone(),
            (None, Some(leaf)) => format!("leaf {leaf}"),
            (None, None) => format!("request {}", self.request_id),
        };
        let guidance = outcome_guidance(&self.outcome);
        match (&self.detail, guidance) {
            (Some(detail), Some(guidance)) if self.outcome == "launch_uncertain" => {
                format!("{subject}: {} — {detail} ({guidance})", self.outcome)
            }
            (Some(detail), _) => format!("{subject}: {} — {detail}", self.outcome),
            (None, Some(guidance)) => format!("{subject}: {} — {guidance}", self.outcome),
            (None, None) => format!("{subject}: {}", self.outcome),
        }
    }
}

/// What an outcome means for the operator, when it is not self-evident.
fn outcome_guidance(outcome: &str) -> Option<&'static str> {
    match outcome {
        "closed_obsolete" => {
            Some("the owner had already ended this claim; decide the task's status on the owner")
        }
        "leaf_running" => Some("still running; it settles itself when it ends"),
        "pending_delivery" | "owner_unreachable" => Some(
            "recorded but not delivered to the owner; the clock sweep retries it, or rerun \
             `orbit run auto --stop` once it is reachable",
        ),
        "pending" => Some("stopped by an error; any later pass retries it"),
        "settlement_refused" => Some(SETTLEMENT_REFUSAL_REMEDY),
        "release_held" => Some(
            "its leaf may still be running, so the owner keeps the claim; the release is \
             delivered once the leaf is seen to stop",
        ),
        "awaiting_drain" => Some("a live drain for this owner will carry it"),
        "unanswered_request" => Some("the owner holds no receipt for it, so nothing is held there"),
        "launch_uncertain" => Some(
            "the launch was never acknowledged, so the leaf may still be running; recover the \
             claim on the owner's dashboard and do not start another attempt",
        ),
        "no_owner_route" => Some("register the owner with `orbit host add <ssh-target>`"),
        _ => None,
    }
}

/// The first wait after the owner refuses a pending settlement while it still
/// holds the claim [ORB-13979]. Each further refusal doubles it, up to
/// [`SETTLEMENT_REFUSAL_MAX_BACKOFF`].
pub(crate) const SETTLEMENT_REFUSAL_FIRST_BACKOFF: std::time::Duration =
    std::time::Duration::from_secs(60);

/// The longest an automatic pass waits between deliveries of a settlement
/// the owner keeps refusing, so a fixed owner still settles it within this.
pub(crate) const SETTLEMENT_REFUSAL_MAX_BACKOFF: std::time::Duration =
    std::time::Duration::from_secs(15 * 60);

/// How long automatic passes wait after the owner refused a settlement
/// `refusals` times in a row.
pub(crate) fn settlement_refusal_backoff(refusals: u32) -> std::time::Duration {
    let doublings = refusals.saturating_sub(1).min(16);
    SETTLEMENT_REFUSAL_FIRST_BACKOFF
        .saturating_mul(1 << doublings)
        .min(SETTLEMENT_REFUSAL_MAX_BACKOFF)
}

/// What an operator does about a settlement the owner keeps refusing.
const SETTLEMENT_REFUSAL_REMEDY: &str = "the owner refuses this recorded outcome while it \
     holds the claim; fix the condition its reason names on the owner. Automatic passes \
     retry with backoff, at most every 15 minutes, and settle it once the owner accepts; \
     `orbit run auto --stop` retries it at once. Recovering the claim on the owner's \
     dashboard closes it instead";

/// A settlement the owner refused while it still holds the claim
/// [ORB-13979]: the record stays `settling`, its outcome kept, and delivery
/// backs off rather than asking again on every pass. `orbit run show` lists
/// these for the drain carrying them, and the drain's passes admit nothing new
/// while any is held for their owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RefusedPullSettlement {
    /// The owner's task the settlement is for.
    pub task_id: Option<String>,
    pub leaf_run_id: Option<String>,
    /// The owner's host-qualified selector.
    pub owner: String,
    /// The drain run that admitted the claim.
    pub drain_run_id: String,
    /// The owner's refusal, as it answered.
    pub reason: String,
    /// Consecutive refused deliveries.
    pub refusals: u32,
    pub first_refused_at: DateTime<Utc>,
    pub last_refused_at: DateTime<Utc>,
    /// No automatic pass delivers it before this.
    pub retry_after: DateTime<Utc>,
    /// What the operator does about it.
    pub remedy: String,
}

impl RefusedPullSettlement {
    /// The refusal `record` is held on, when it is a `settling` admission
    /// the owner refused.
    pub(crate) fn of(record: &LocalPullAdmission) -> Option<Self> {
        if record.phase != LocalPullPhase::Settling {
            return None;
        }
        let refusal = record.settlement_refusal.as_ref()?;
        Some(Self {
            task_id: record
                .receipt
                .as_ref()
                .and_then(|receipt| receipt.claim.as_ref())
                .map(|claim| claim.task_id.clone()),
            leaf_run_id: record.leaf_run_id.clone(),
            owner: record.destination.selector.clone(),
            drain_run_id: record.request.run_context.run_id.clone(),
            reason: refusal.reason.clone(),
            refusals: refusal.refusals,
            first_refused_at: refusal.first_refused_at,
            last_refused_at: refusal.last_refused_at,
            retry_after: refusal.retry_after,
            remedy: SETTLEMENT_REFUSAL_REMEDY.to_string(),
        })
    }

    /// One line for a terminal report.
    #[must_use]
    pub fn describe(&self) -> String {
        format!(
            "{} (leaf {}) owner {} refused it {} time(s) since {}: {}; next attempt after {} — {}",
            self.task_id.as_deref().unwrap_or("-"),
            self.leaf_run_id.as_deref().unwrap_or("-"),
            self.owner,
            self.refusals,
            self.first_refused_at.to_rfc3339(),
            self.reason,
            self.retry_after.to_rfc3339(),
            self.remedy
        )
    }
}

/// Settlements this follower recorded but never delivered to the owner.
///
/// A live or cancelling drain retries delivery on every pass, and a leaf's
/// own worker retries its settlement with a bounded backoff when no drain
/// carries its owner. What outlasts both is retried by the OS clock sweep,
/// or waits for the next drain or for an operator to run
/// `orbit run auto --stop` on a host without the clock. This is the
/// read-only summary `orbit doctor` reports so the wait is visible.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PendingPullSettlements {
    /// Admissions whose outcome is recorded but not delivered.
    pub count: usize,
    /// When the oldest such admission's leaf ended, and so when its outcome
    /// was recorded. `None` when none is pending or no ended leaf run can be
    /// read for any of them.
    pub oldest_recorded_at: Option<DateTime<Utc>>,
}

impl PendingPullSettlements {
    /// How long the oldest pending settlement has waited, at `now`.
    #[must_use]
    pub fn oldest_age(&self, now: DateTime<Utc>) -> Option<chrono::Duration> {
        self.oldest_recorded_at
            .map(|recorded| (now - recorded).max(chrono::Duration::zero()))
    }
}

/// The owner claim a local pull leaf run executes, as this follower recorded
/// it. `orbit run show <leaf-run>` reads this so a leaf's run page names the
/// task it works for, which owner holds the claim, and whether the outcome has
/// reached that owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PullLeafClaim {
    /// The owner's task the leaf executes.
    pub task_id: Option<String>,
    /// The owner's claim on that task.
    pub claim_id: Option<String>,
    /// The owner's host-qualified selector.
    pub owner: String,
    /// The drain run that admitted the claim.
    pub drain_run_id: String,
    /// Where the follower's record of this admission stands (`launched`,
    /// `settling`, `settled`, ...).
    pub settlement_phase: String,
    /// Why the owner refused the settlement, once it had already ended the
    /// claim.
    pub refusal: Option<String>,
    /// The owner's refusal of the pending settlement while it still holds
    /// the claim, and when delivery is next attempted.
    pub settlement_refusal: Option<RefusedPullSettlement>,
    /// [ORB-14257] Why the leaf ended without its handoff, as its recorded
    /// settlement types it; `None` until a failure or release is recorded,
    /// and for a release of a leaf that never launched.
    pub failure_class: Option<ClaimFailureClass>,
    /// What the phase means for the operator.
    pub guidance: String,
}

impl PullLeafClaim {
    /// One line for a run page.
    #[must_use]
    pub fn describe(&self) -> String {
        let failure = self
            .failure_class
            .map(|class| format!(" failure={}", class.as_str()))
            .unwrap_or_default();
        format!(
            "task {} claim {} owner {} drain {} settlement={}{failure} — {}",
            self.task_id.as_deref().unwrap_or("-"),
            self.claim_id.as_deref().unwrap_or("-"),
            self.owner,
            self.drain_run_id,
            self.settlement_phase,
            self.guidance
        )
    }
}

/// A claimed leaf a pull drain is still carrying: launched, not yet
/// terminal. `orbit run cancel` (graceful) waits for these, `--force` stops
/// them, and `orbit run auto --stop` and `orbit run show` list them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DrainClaimedLeaf {
    pub leaf_run_id: String,
    /// The leaf's job: one of the claimed leaf definitions.
    pub job_id: String,
    /// The owner's task the leaf executes.
    pub task_id: Option<String>,
    /// The owner's host-qualified selector.
    pub owner: String,
    /// The drain run that admitted the claim; an earlier drain's admission
    /// is carried by the live drain for the same owner.
    pub admitted_by: String,
    /// The leaf run's state (`running`, `pending`, ...).
    pub leaf_state: String,
    /// Where the follower's record of this admission stands.
    pub settlement_phase: String,
    /// Whether the leaf is still implementing or has reached its
    /// before-landing review [ORB-15194].
    pub stage: ClaimedLeafStage,
}

/// What a live claimed leaf is doing with its drain slot [ORB-15194].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimedLeafStage {
    /// Implementing, validating or publishing: the work the slot exists for.
    Implementing,
    /// Past `pr_open`, in the before-landing review of its open pull request
    /// and what follows it. The drain admits a replacement beside it, up to
    /// its concurrency.
    Reviewing,
}

impl ClaimedLeafStage {
    fn as_str(self) -> &'static str {
        match self {
            Self::Implementing => "implementing",
            Self::Reviewing => "reviewing",
        }
    }
}

impl DrainClaimedLeaf {
    /// One line for a terminal report.
    #[must_use]
    pub fn describe(&self) -> String {
        format!(
            "{} (leaf {}) {} {} settlement={}",
            self.task_id.as_deref().unwrap_or("-"),
            self.leaf_run_id,
            self.leaf_state,
            self.stage.as_str(),
            self.settlement_phase
        )
    }
}
