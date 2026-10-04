//! What a follower's settle-only pass did with each pull admission
//! [ORB-13663].
//!
//! Settlement no longer belongs to the drain that admitted a claim. The
//! admission record is the outbox: the leaf's own worker records and delivers
//! its settlement as it terminalizes, and any other follower process —
//! `orbit run cancel`, `orbit run auto --stop`, a new drain — delivers whatever
//! is still recorded. This is the report those surfaces print, one entry per
//! admission that still held a slot when the pass started.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
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
    /// - `settled` — the owner accepted its handoff or failure;
    /// - `closed_obsolete` — the owner had already ended the claim, so the
    ///   settlement was closed locally (ORB-13639);
    /// - `leaf_running` — its leaf is live and settles itself when it ends;
    /// - `pending_delivery` — a settlement is recorded but did not reach the
    ///   owner; any later pass retries it;
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
            "recorded but not delivered to the owner; rerun `orbit run auto --stop` once it is \
             reachable",
        ),
        "pending" => Some("stopped by an error; any later pass retries it"),
        "awaiting_drain" => Some("a live drain for this owner will carry it"),
        "unanswered_request" => Some("the owner holds no receipt for it, so nothing is held there"),
        "launch_uncertain" => Some(
            "the launch was never acknowledged, so the leaf may still be running; recover the \
             claim on the owner's dashboard and do not start another attempt",
        ),
        "no_owner_route" => Some("add the owner to ~/.orbit/mcp-destinations.toml"),
        _ => None,
    }
}

/// Settlements this follower recorded but never delivered to the owner.
///
/// Nothing retries delivery on a timer by design, so an undelivered
/// settlement waits for an operator to run `orbit run auto --stop`. This is
/// the read-only summary `orbit doctor` reports so the wait is visible.
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
    /// What the phase means for the operator.
    pub guidance: String,
}

impl PullLeafClaim {
    /// One line for a run page.
    #[must_use]
    pub fn describe(&self) -> String {
        format!(
            "task {} claim {} owner {} drain {} settlement={} — {}",
            self.task_id.as_deref().unwrap_or("-"),
            self.claim_id.as_deref().unwrap_or("-"),
            self.owner,
            self.drain_run_id,
            self.settlement_phase,
            self.guidance
        )
    }
}

fn phase_guidance(phase: orbit_store::contracts::LocalPullPhase, refusal: Option<&str>) -> String {
    use orbit_store::contracts::LocalPullPhase as Phase;
    match (phase, refusal) {
        (Phase::Settled, Some(refusal)) => format!(
            "the owner had already ended this claim ({refusal}); decide the task's status on \
             the owner"
        ),
        (Phase::Settled, None) => "the outcome was delivered to the owner".to_string(),
        (Phase::Settling, _) => "the outcome is recorded but not delivered to the owner; \
             `orbit run auto --stop` retries delivery"
            .to_string(),
        (Phase::Launched, _) => {
            "the leaf settles itself when it ends; nothing to do while it runs".to_string()
        }
        (Phase::Launching, _) => "the launch was never acknowledged; recover the claim on the \
             owner's dashboard rather than starting another attempt"
            .to_string(),
        _ => "admitted, not launched yet".to_string(),
    }
}

impl crate::OrbitRuntime {
    /// The pull admission a local run executes, when it is a claimed leaf.
    /// `None` for every other run, including runs on an owner checkout.
    pub fn pull_leaf_claim(
        &self,
        run_id: &str,
    ) -> Result<Option<PullLeafClaim>, orbit_common::OrbitError> {
        let Some(admission) = self.claimed_leaf_admission(run_id)? else {
            return Ok(None);
        };
        let claim = admission
            .receipt
            .as_ref()
            .and_then(|receipt| receipt.claim.as_ref());
        let settlement_phase = serde_json::to_value(admission.phase)
            .ok()
            .and_then(|value| value.as_str().map(str::to_string))
            .unwrap_or_else(|| "unknown".to_string());
        Ok(Some(PullLeafClaim {
            task_id: claim.map(|claim| claim.task_id.clone()),
            claim_id: claim.map(|claim| claim.claim_id.clone()),
            owner: admission.destination.selector.clone(),
            drain_run_id: admission.request.run_context.run_id.clone(),
            settlement_phase,
            refusal: admission.refusal.clone(),
            guidance: phase_guidance(admission.phase, admission.refusal.as_deref()),
        }))
    }
}

/// Whether the owner answered a pull with a refusal, as opposed to a lost or
/// uncertain delivery.
///
/// Only an answer counts. A refusal from the owner's pre-admission ladder —
/// selector, capability, shape, version, ship mode, review policy, a stale
/// ship contract — commits nothing, so it is safe to close the request once
/// the owner also confirms it holds no receipt for it. A delivery miss, a lost
/// answer or a store failure says nothing about whether an earlier send of the
/// same request committed, so the request stays pending and is retried.
pub(crate) fn is_owner_refusal(error: &OrbitError) -> bool {
    match error {
        OrbitError::RemoteTool { code, .. } => matches!(
            code.as_str(),
            "invalid_input" | "capability_refused" | "capability_denied" | "policy_denied"
        ),
        OrbitError::InvalidInput(_)
        | OrbitError::CapabilityRefused(_)
        | OrbitError::CapabilityDenied(_)
        | OrbitError::PolicyDenied(_) => true,
        _ => false,
    }
}

/// Whether an error says the owner could not be reached or did not answer
/// cleanly: an unreachable or stale route, a lost or unknown outcome, an owner
/// that is unavailable, or an owner-side failure that is not a refusal.
///
/// Only these mean the next call to the same owner is likely to fail or hang
/// the same way, so a pass stops calling that owner after the first one. A
/// refusal is an answer, and a local error (a store read, a missing binding)
/// says nothing about the owner at all.
pub(crate) fn is_owner_transport_failure(error: &OrbitError) -> bool {
    match error {
        OrbitError::RemoteTool { .. } => !is_owner_refusal(error),
        OrbitError::UnreachableDestination(_)
        | OrbitError::OutcomeUnknown { .. }
        | OrbitError::OwnerUnavailable(_)
        | OrbitError::OwnerNegotiation(_)
        | OrbitError::StaleRoute(_)
        | OrbitError::UnhealthyCheckout(_) => true,
        _ => false,
    }
}
