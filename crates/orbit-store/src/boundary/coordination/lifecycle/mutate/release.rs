//! The decision for a claim release: whether the release counts against the
//! task's budget, whether it blocks the task, and where the task goes next.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::task::TaskStatus;
use orbit_types::workflow::{ClaimFailureClass, ReviewGateHistoryEvent, ReviewGateHistoryNote};

use super::super::super::TaskCommitBoundary;
use super::super::codec::{
    OWNER_BUSY_RELEASE_ALLOWANCE, RELEASE_BUDGET, RELEASE_BUDGET_WINDOW_HOURS,
};
use super::super::invalid;
use super::super::releases::release_budget_comment;
use super::decide::{ClaimDecision, leaf_run_evidence_row, stale_claim};
use crate::contract::*;

/// Why a release blocks the task instead of returning it to the backlog.
enum ReleaseBlock {
    /// The failure's class needs an operator, or cannot be retried.
    Class,
    /// The task's release budget is spent; the comment says how.
    BudgetExhausted(String),
}

impl TaskCommitBoundary {
    pub(super) fn decide_release(
        &self,
        auth: &ClaimInvocation,
        value: &ClaimEvidence,
        decision: &mut ClaimDecision,
    ) -> Result<(), OrbitError> {
        if auth.operator
            || !matches!(
                decision.claim.phase,
                ExecutionClaimPhase::Claimed | ExecutionClaimPhase::Running
            )
        {
            return Err(stale_claim());
        }
        let Some(reason) = value.summary.as_deref().filter(|s| !s.trim().is_empty()) else {
            return Err(invalid("claim release requires a reason"));
        };
        let released_at = Utc::now();
        let failure = self.release_failure(&decision.claim, value, released_at)?;
        let block = self.release_block(&decision.claim, failure.as_ref(), released_at)?;
        let failed_note =
            self.settle_release(auth, value, failure.as_ref(), &block, released_at, decision)?;
        match block {
            Some(block) => {
                let operator_decision = failure
                    .as_ref()
                    .is_some_and(|failure| failure.class.requires_operator_decision());
                hold_blocked_release(
                    block,
                    operator_decision,
                    reason,
                    value,
                    failed_note,
                    decision,
                );
            }
            None => return_released_task(auth, value, reason, decision),
        }
        Ok(())
    }

    /// A busy owner exempts a release only for a `transient` failure, and
    /// only for the task's first few such releases in the budget window; any
    /// other is an ordinary release.
    fn release_failure(
        &self,
        claim: &ExecutionClaim,
        value: &ClaimEvidence,
        released_at: DateTime<Utc>,
    ) -> Result<Option<ClaimFailure>, OrbitError> {
        Ok(match &value.failure {
            Some(failure) if failure.owner_busy => {
                let exempt = failure.class == ClaimFailureClass::Transient
                    && self.owner_busy_releases(&claim.task_id, released_at)?.len()
                        < OWNER_BUSY_RELEASE_ALLOWANCE;
                Some(ClaimFailure {
                    owner_busy: exempt,
                    ..failure.clone()
                })
            }
            failure => failure.clone(),
        })
    }

    /// [ORB-14257] A typed failure releases only when its class does not need
    /// an operator and the task's release budget allows it. A provider usage
    /// limit is not counted [ORB-14695].
    fn release_block(
        &self,
        claim: &ExecutionClaim,
        failure: Option<&ClaimFailure>,
        released_at: DateTime<Utc>,
    ) -> Result<Option<ReleaseBlock>, OrbitError> {
        Ok(match failure {
            Some(failure)
                if failure.class.blocks() || failure.class.requires_operator_decision() =>
            {
                Some(ReleaseBlock::Class)
            }
            Some(failure) if failure.budgeted() => {
                let earlier = self.budgeted_releases(&claim.task_id, released_at)?;
                (earlier.len() >= RELEASE_BUDGET).then(|| {
                    ReleaseBlock::BudgetExhausted(release_budget_comment(
                        &earlier,
                        failure,
                        &released_at,
                    ))
                })
            }
            _ => None,
        })
    }

    /// Record the release as the claim's settlement, with the leaf's evidence
    /// and the candidate it leaves behind. Returns the note that names a
    /// failure when the release blocks the task.
    fn settle_release(
        &self,
        auth: &ClaimInvocation,
        value: &ClaimEvidence,
        failure: Option<&ClaimFailure>,
        block: &Option<ReleaseBlock>,
        released_at: DateTime<Utc>,
        decision: &mut ClaimDecision,
    ) -> Result<String, OrbitError> {
        let ClaimDecision {
            claim,
            state,
            params,
            release,
            ..
        } = decision;
        state.release = failure.map(|failure| ClaimReleaseRecord {
            class: failure.class,
            reason: failure.reason.clone(),
            released_at: released_at.to_rfc3339(),
            budget_exhausted: matches!(block, Some(ReleaseBlock::BudgetExhausted(_))),
            forge_unavailable: value.forge_hold.is_some(),
            provider_limit: failure.provider_limit,
            owner_busy: failure.owner_busy,
            delivery_hold: failure.delivery_hold,
        });
        let settlement = ClaimSettlementRecord::of(
            ClaimSettlementKind::Release,
            value,
            &auth.attribution,
            released_at.to_rfc3339(),
        );
        let failed_note =
            ClaimFailedNote::of(&settlement, &claim.executed_on, state.bound_run.as_ref()).text();
        state.settlement = Some(settlement);
        params.rows.extend(leaf_run_evidence_row(
            claim,
            state,
            value.leaf_run.as_ref(),
        )?);
        state.preserved_candidate = self.preserve_candidate(&claim.task_id, value)?;
        params.append_history.extend(Self::kept_candidate_entry(
            claim,
            state.preserved_candidate.as_ref(),
        ));
        state.landing_invalidated = true;
        *release = true;
        Ok(failed_note)
    }
}

/// A release that blocks the task: it fails the claim and holds the task for
/// an operator, or for the budget's exhaustion.
fn hold_blocked_release(
    block: ReleaseBlock,
    operator_decision: bool,
    reason: &str,
    value: &ClaimEvidence,
    failed_note: String,
    decision: &mut ClaimDecision,
) {
    let ClaimDecision {
        evidence,
        state,
        params,
        ..
    } = decision;
    let (exhausted, comment) = match block {
        ReleaseBlock::BudgetExhausted(comment) => (true, Some(comment)),
        // Older followers may still describe this release as returning to
        // backlog. The owner's hold is authoritative.
        ReleaseBlock::Class if operator_decision => (
            false,
            Some(format!(
                "Task held for an operator decision before another attempt: {reason}"
            )),
        ),
        ReleaseBlock::Class => (false, value.comment.clone()),
    };
    *evidence = ClaimEvidence {
        comment,
        ..ClaimEvidence::default()
    };
    state.claim.phase = ExecutionClaimPhase::Failed;
    params.status = Some(TaskStatus::Blocked);
    params.status_note = Some(if exhausted {
        format!(
            "release budget exhausted ({RELEASE_BUDGET} in \
             {RELEASE_BUDGET_WINDOW_HOURS}h): {reason}"
        )
    } else {
        failed_note
    });
    state.last_event = if exhausted {
        "claim_release_budget_exhausted"
    } else {
        "claim_failed"
    }
    .into();
}

/// A release that returns the task to the backlog untouched: its execution
/// summary stays whatever the last real attempt left, and the reason travels
/// as the status note and the comment.
fn return_released_task(
    auth: &ClaimInvocation,
    value: &ClaimEvidence,
    reason: &str,
    decision: &mut ClaimDecision,
) {
    let ClaimDecision {
        evidence,
        state,
        params,
        ..
    } = decision;
    *evidence = ClaimEvidence {
        comment: value.comment.clone(),
        ..ClaimEvidence::default()
    };
    state.claim.phase = ExecutionClaimPhase::Revoked;
    params.status = Some(TaskStatus::Backlog);
    // A release for a review evidence hold keeps the task in
    // progress under the hold, the latest decision, so
    // receipt of the named evidence queues a fresh review.
    // The note names the held run as a local hold's does.
    if let Some(hold) = &value.evidence_hold {
        params.status = None;
        params.status_note = Some(ReviewGateHistoryNote::awaiting_evidence_text(
            &hold.run_id,
            &hold.candidate.commit,
            Some(reason),
        ));
        state.last_event = ReviewGateHistoryEvent::AwaitingEvidence.as_str().into();
    // [ORB-14258] A release for a red base is recorded as the
    // hold itself, so owner admission withholds the task until
    // the command passes on a new base tip.
    } else if let Some(hold) = &value.baseline_red {
        params.status_note = Some(hold.text(reason));
        params.status_decision = Some(orbit_types::task::TaskDecision::BaselineRedHold(
            hold.clone(),
        ));
        state.last_event = orbit_types::workflow::BASELINE_RED_HOLD_EVENT.into();
    // [ORB-15573] A release its leaf's provider refused is
    // recorded as the hold the owner computed, so owner and
    // pull admission draw the task onto another provider.
    } else if let Some(hold) = &auth.provider_hold {
        params.status_note = Some(hold.text(reason));
        state.last_event = orbit_types::workflow::PROVIDER_FAILURE_HOLD_EVENT.into();
    } else {
        params.status_note = Some(reason.to_string());
        state.last_event = "claim_released".into();
    }
}
