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
        if receipt.input != input {
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
