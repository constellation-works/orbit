use chrono::Utc;
use orbit_common::fs::io::hold_step;
use orbit_common::{ClaimRefusalKind, OrbitError};

use super::super::super::TaskCommitBoundary;
use super::super::codec::{CLAIM, RECEIPT, STATE};
use super::super::{ClaimAuthority, MutationReceipt, invalid, row};
use super::decide::ClaimDecision;
use crate::contract::*;

/// Steps of a claim mutation's admission section, so a long hold's report
/// names where it went rather than the section's whole operation.
const RECEIPT_STEP: &str = "claim receipt";
const AUTHORITY_STEP: &str = "claim authority";
const DECISION_STEP: &str = "claim decision";
const COMMIT_STEP: &str = "claim commit";

/// A decided claim mutation, laid out as the one task commit that applies it.
struct PreparedClaimCommit {
    params: TaskCoordinationCommitParams,
    effects: ClaimCommitEffects,
    evidence: ClaimEvidence,
    binding: Option<ClaimRun>,
    result: ClaimMutationResult,
}

impl TaskCommitBoundary {
    pub(super) fn mutate_claim_locked(
        &self,
        auth: &ClaimInvocation,
        mutation: &ClaimMutation,
        receipt_id: &str,
        input: String,
    ) -> Result<ClaimMutationOutcome, OrbitError> {
        if let Some(result) = hold_step(RECEIPT_STEP, || {
            self.replay_claim_receipt(auth, mutation, receipt_id, &input)
        })? {
            return Ok(ClaimMutationOutcome {
                result,
                committed: false,
            });
        }
        let ClaimAuthority {
            row: old,
            claim,
            state,
            state_row: old_state,
            bundle,
            expected_status,
        } = hold_step(AUTHORITY_STEP, || {
            self.claim_authority(auth, matches!(mutation, ClaimMutation::Recover { .. }))
        })?;
        let mut decision = ClaimDecision::new(auth, claim, state, bundle, expected_status);
        hold_step(DECISION_STEP, || {
            self.decide_claim_mutation(auth, mutation, receipt_id, &mut decision)
        })?;
        let prepared =
            self.prepare_claim_commit(auth, receipt_id, input, decision, old, old_state)?;
        let outcome = hold_step(COMMIT_STEP, || {
            self.commit_locked_effects(
                &prepared.params,
                &mut |_| Ok(prepared.params.rows.clone()),
                &prepared.effects,
                &prepared.evidence,
                prepared.binding.as_ref(),
            )
        })?;
        self.finish_claim_mutation(mutation, receipt_id, prepared.result, outcome)
    }

    /// Lay a decision out as rows, replacements and effects of one commit,
    /// with the mutation's receipt and the claim's new state beside them.
    fn prepare_claim_commit(
        &self,
        auth: &ClaimInvocation,
        receipt_id: &str,
        input: String,
        decision: ClaimDecision,
        old: TaskCoordinationRow,
        old_state: Option<TaskCoordinationRow>,
    ) -> Result<PreparedClaimCommit, OrbitError> {
        let ClaimDecision {
            claim,
            expected_status,
            mut state,
            mut params,
            mut evidence,
            binding,
            release,
            history_event,
            landing_comments,
            handoff_effects,
            ..
        } = decision;
        state.updated_at = Utc::now().to_rfc3339();
        state.age_seconds = None;
        params.status_event = history_event.then(|| state.last_event.clone());
        let result = ClaimMutationResult {
            failure_revision: None,
            claim_id: claim.claim_id.clone(),
            phase: state.claim.phase,
            status: params.status.unwrap_or(expected_status),
            friction: None,
        };
        params.rows.push(row(
            RECEIPT,
            receipt_id,
            &MutationReceipt {
                input,
                result: result.clone(),
            },
        )?);
        let new_state = row(STATE, &claim.claim_id, &state)?;
        let mut effects = claim_commit_effects(
            auth,
            &claim,
            &state,
            release,
            landing_comments,
            handoff_effects,
        );
        effects
            .replacements
            .push((old, row(CLAIM, &claim.claim_id, &state.claim)?));
        if let Some(old_state) = old_state {
            effects.replacements.push((old_state, new_state));
        } else {
            params.rows.push(new_state);
        }
        // A phase that still protects the task (including repair admission,
        // which revokes and re-claims in its own commit) must not receive
        // observations queued by the sweep. Once this mutation leaves the
        // task unprotected, land them in the same commit.
        if !state.claim.phase.protects_footprint() {
            self.attach_deferred_branch_observations(&claim.task_id, &mut evidence, &mut effects)?;
        }
        Ok(PreparedClaimCommit {
            params,
            effects,
            evidence,
            binding,
            result,
        })
    }

    /// The outcome a committed mutation reports; a commit that did not land
    /// means the claim moved on underneath it.
    fn finish_claim_mutation(
        &self,
        mutation: &ClaimMutation,
        receipt_id: &str,
        result: ClaimMutationResult,
        outcome: TaskCoordinationCommitOutcome,
    ) -> Result<ClaimMutationOutcome, OrbitError> {
        match outcome {
            TaskCoordinationCommitOutcome::Committed(_) => {
                let result = if matches!(mutation, ClaimMutation::Fail(_)) {
                    // The commit stamps the owner-measured failure revision on
                    // the receipt, so the winner and a replay use one baseline.
                    let receipt = self
                        .coordination_row(RECEIPT, receipt_id)?
                        .ok_or_else(|| invalid("committed claim receipt missing"))?;
                    super::super::decode::<MutationReceipt>(&receipt.payload_json)?.result
                } else {
                    result
                };
                Ok(ClaimMutationOutcome {
                    result: self.with_friction_result(result, receipt_id)?,
                    committed: true,
                })
            }
            _ => Err(OrbitError::claim_refused(ClaimRefusalKind::StaleClaim)),
        }
    }
}

/// The effects a decided mutation publishes with its commit, beside the
/// replacements for the claim's own rows.
fn claim_commit_effects(
    auth: &ClaimInvocation,
    claim: &ExecutionClaim,
    state: &ClaimInspection,
    release: bool,
    landing_comments: Vec<String>,
    handoff_effects: ClaimCommitEffects,
) -> ClaimCommitEffects {
    ClaimCommitEffects {
        replacements: handoff_effects.replacements,
        release_reservation: release.then(|| claim.reservation_id.clone()),
        friction: handoff_effects.friction,
        execution_origin: Some(claim.executed_on.clone()),
        worker_update: handoff_effects.worker_update,
        regression_from: handoff_effects.regression_from,
        // `claim_authority` matched a worker's machine and run to this
        // binding; an operator's mutation carries no worker evidence.
        claimed_submission: state
            .bound_run
            .clone()
            .filter(|_| !auth.operator)
            .map(|run| (claim.claim_id.clone(), run)),
        census: None,
        observation_comments: landing_comments,
    }
}
