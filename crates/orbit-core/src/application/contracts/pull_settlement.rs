//! What a settle-only pass reports for each pull admission it carried
//! [ORB-13663]. Job surfaces print these and the distributed settlement
//! records them, so neither owns the shape.

use serde::Serialize;

/// Result of a settle-only pass for one admission.
///
/// Serialized as the existing snake_case response tokens. Unknown response
/// tokens are rejected on deserialization; persisted audit records remain separate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PullSettlementOutcome {
    Settled,
    ClosedObsolete,
    LeafRunning,
    PendingDelivery,
    SettlementRefused,
    ReleaseHeld,
    OwnerUnreachable,
    AwaitingDrain,
    UnansweredRequest,
    LaunchUncertain,
    Idle,
    Refused,
    Pending,
    NoOwnerRoute,
}

impl PullSettlementOutcome {
    /// The stable response and audit token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Settled => "settled",
            Self::ClosedObsolete => "closed_obsolete",
            Self::LeafRunning => "leaf_running",
            Self::PendingDelivery => "pending_delivery",
            Self::SettlementRefused => "settlement_refused",
            Self::ReleaseHeld => "release_held",
            Self::OwnerUnreachable => "owner_unreachable",
            Self::AwaitingDrain => "awaiting_drain",
            Self::UnansweredRequest => "unanswered_request",
            Self::LaunchUncertain => "launch_uncertain",
            Self::Idle => "idle",
            Self::Refused => "refused",
            Self::Pending => "pending",
            Self::NoOwnerRoute => "no_owner_route",
        }
    }
}

impl std::fmt::Display for PullSettlementOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

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
    pub outcome: PullSettlementOutcome,
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
        let guidance = outcome_guidance(self.outcome);
        match (&self.detail, guidance) {
            (Some(detail), Some(guidance))
                if self.outcome == PullSettlementOutcome::LaunchUncertain =>
            {
                format!("{subject}: {} — {detail} ({guidance})", self.outcome)
            }
            (Some(detail), _) => format!("{subject}: {} — {detail}", self.outcome),
            (None, Some(guidance)) => format!("{subject}: {} — {guidance}", self.outcome),
            (None, None) => format!("{subject}: {}", self.outcome),
        }
    }
}

/// What an outcome means for the operator, when it is not self-evident.
fn outcome_guidance(outcome: PullSettlementOutcome) -> Option<&'static str> {
    match outcome {
        PullSettlementOutcome::ClosedObsolete => {
            Some("the owner had already ended this claim; decide the task's status on the owner")
        }
        PullSettlementOutcome::LeafRunning => Some("still running; it settles itself when it ends"),
        PullSettlementOutcome::PendingDelivery | PullSettlementOutcome::OwnerUnreachable => Some(
            "recorded but not delivered to the owner; the clock sweep retries it, or rerun \
             `orbit run auto --stop` once it is reachable",
        ),
        PullSettlementOutcome::Pending => Some("stopped by an error; any later pass retries it"),
        PullSettlementOutcome::SettlementRefused => Some(SETTLEMENT_REFUSAL_REMEDY),
        PullSettlementOutcome::ReleaseHeld => Some(
            "its leaf may still be running, so the owner keeps the claim; the release is \
             delivered once the leaf is seen to stop",
        ),
        PullSettlementOutcome::AwaitingDrain => Some("a live drain for this owner will carry it"),
        PullSettlementOutcome::UnansweredRequest => {
            Some("the owner holds no receipt for it, so nothing is held there")
        }
        PullSettlementOutcome::LaunchUncertain => Some(
            "the launch was never acknowledged, so the leaf may still be running; recover the \
             claim on the owner's dashboard and do not start another attempt",
        ),
        PullSettlementOutcome::NoOwnerRoute => {
            Some("register the owner with `orbit host add <ssh-target>`")
        }
        PullSettlementOutcome::Settled
        | PullSettlementOutcome::Idle
        | PullSettlementOutcome::Refused => None,
    }
}

/// What an operator does about a settlement the owner keeps refusing.
pub(in crate::application) const SETTLEMENT_REFUSAL_REMEDY: &str = "the owner refuses this recorded outcome while it \
     holds the claim; fix the condition its reason names on the owner. Automatic passes \
     retry with backoff, at most every 15 minutes, and settle it once the owner accepts; \
     `orbit run auto --stop` retries it at once. Recovering the claim on the owner's \
     dashboard closes it instead";
