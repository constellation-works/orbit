use super::super::super::TaskCommitBoundary;
use super::super::codec::{CLAIM, RECEIPT};
use super::super::{MutationReceipt, decode, invalid};
use crate::contracts::*;
use orbit_common::{ClaimRefusalKind, OrbitError};

impl TaskCommitBoundary {
    /// The recorded outcome of the mutation `receipt_id` names, or `None`
    /// when it has none. A receipt recorded for different input is refused.
    pub(super) fn replay_claim_receipt(
        &self,
        auth: &ClaimInvocation,
        mutation: &ClaimMutation,
        receipt_id: &str,
        input: &str,
    ) -> Result<Option<ClaimMutationResult>, OrbitError> {
        let Some(receipt) = self.coordination_row(RECEIPT, receipt_id)? else {
            return Ok(None);
        };
        let receipt: MutationReceipt = decode(&receipt.payload_json)?;
        if !receipt_input_matches(&receipt.input, input, mutation) {
            return Err(invalid("mutation_mismatch"));
        }
        if let ClaimMutation::RetryLanding(request) = mutation {
            // Replaying a reservation whose submission is unknown is launch
            // admission, so historical approval cannot resurrect revoked work.
            let authority = self.claim_authority(auth, false)?;
            self.verify_reserved_landing_retry(auth, &authority.state, request)?;
        }
        if matches!(mutation, ClaimMutation::ReserveBaseUpdate(_)) {
            return Err(invalid(
                "base update reservation already recorded; observe its outcome instead of repeating a provider write",
            ));
        }
        // A persisted send intent is uncertainty, never permission to send again.
        // The landing consumer must reconcile external state, including after a
        // later revocation or a changed candidate. Do not replay launch authority.
        if matches!(
            mutation,
            ClaimMutation::MergeIntent {
                resolved: false,
                ..
            }
        ) {
            return Err(OrbitError::claim_refused(
                ClaimRefusalKind::MergeIntentReplayUnreconciled,
            ));
        }
        // Binding is also a launch gate. Never replay historical launch permission
        // after handoff, failure, or revocation; other receipts are advisory outcomes.
        if let ClaimMutation::Bind { run, .. } = mutation {
            let current: ExecutionClaim = self
                .coordination_row(CLAIM, &auth.claim_id)?
                .map(|row| decode(&row.payload_json))
                .transpose()?
                .ok_or_else(|| OrbitError::claim_refused(ClaimRefusalKind::StaleClaim))?;
            if current.phase != ExecutionClaimPhase::Running
                || self.claim_state(current)?.bound_run.as_ref() != Some(run)
            {
                return Err(OrbitError::claim_refused(ClaimRefusalKind::StaleClaim));
            }
        }
        self.with_friction_result(receipt.result, receipt_id)
            .map(Some)
    }
}

/// Whether `input` is the input `recorded` was written for. Recovery
/// receipts written before `expected_phase` joined the recovery mutation
/// recorded no phase, so a recovery replays one of those when every other
/// part of the input matches; the phase the original request observed is
/// unrecoverable from such a receipt. Receipts written since then match
/// exactly, including the phase.
fn receipt_input_matches(recorded: &str, input: &str, mutation: &ClaimMutation) -> bool {
    if recorded == input {
        return true;
    }
    if !matches!(mutation, ClaimMutation::Recover { .. }) {
        return false;
    }
    let Ok(mut current) = serde_json::from_str::<serde_json::Value>(input) else {
        return false;
    };
    let Ok(recorded) = serde_json::from_str::<serde_json::Value>(recorded) else {
        return false;
    };
    let Some(recover) = current
        .as_array_mut()
        .and_then(|tuple| tuple.last_mut())
        .and_then(|mutation| mutation.get_mut("Recover"))
        .and_then(serde_json::Value::as_object_mut)
    else {
        return false;
    };
    recover.remove("expected_phase");
    current == recorded
}
