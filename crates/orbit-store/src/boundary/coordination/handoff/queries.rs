//! Read access to accepted handoffs and captured claim contracts.
use orbit_common::OrbitError;
use orbit_types::workflow::{ReviewCertificate, handoff::*};

use super::super::lifecycle::{decode, invalid};
use super::{HANDOFF, START, TaskCommitBoundary};
use crate::contracts::*;

impl TaskCommitBoundary {
    pub fn landing_start_requests(&self) -> Result<Vec<LandingStartRequest>, OrbitError> {
        self.enter_ordinary(|| {
            self.coordination_rows(START)?
                .iter()
                .map(|r| decode(&r.payload_json))
                .collect()
        })
    }

    /// Every handoff this owner accepted, one per claim. Read-only; delivery
    /// attribution matches a landed pull request against these to name the
    /// task it delivered.
    pub fn accepted_handoffs(&self) -> Result<Vec<AcceptedHandoff>, OrbitError> {
        self.enter_ordinary(|| {
            self.coordination_rows(HANDOFF)?
                .iter()
                .map(|r| decode(&r.payload_json))
                .collect()
        })
    }

    pub fn accepted_handoff(&self, claim_id: &str) -> Result<AcceptedHandoff, OrbitError> {
        self.find_accepted_handoff(claim_id)?
            .ok_or_else(|| invalid("typed handoff unavailable"))
    }

    /// The claim's accepted handoff, or `None` when none was accepted, so
    /// callers can tell an absent handoff from a failed read.
    pub fn find_accepted_handoff(
        &self,
        claim_id: &str,
    ) -> Result<Option<AcceptedHandoff>, OrbitError> {
        self.coordination_row(HANDOFF, claim_id)?
            .map(|r| decode(&r.payload_json))
            .transpose()
    }

    /// The certificate an accepted before-PR handoff carries, read back
    /// through the digest the handoff pinned, or `None` for a handoff that
    /// carries no before-PR review [ORB-13895].
    pub fn accepted_review_certificate(
        &self,
        claim_id: &str,
    ) -> Result<Option<ReviewCertificate>, OrbitError> {
        self.enter_ordinary(|| {
            let Some(accepted) = self.find_accepted_handoff(claim_id)? else {
                return Ok(None);
            };
            let Some(evidence) = accepted.handoff.review.evidence() else {
                return Ok(None);
            };
            let bytes = self.artifact_bytes(&accepted.handoff.task_id, &evidence.certificate)?;
            serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|e| invalid(&format!("invalid review certificate: {e}")))
        })
    }

    /// The ship contract the claim was admitted under.
    pub(super) fn claim_ship(
        &self,
        state: &ClaimInspection,
    ) -> Result<AdmissionShipContract, OrbitError> {
        let AdmissionLookup::Found { receipt, .. } = self.lookup_admission(
            &AdmissionIdentity::trusted_local(state.claim.executed_on.clone()),
            &state.claim.request_id,
        )?
        else {
            return Err(invalid("claim receipt unavailable"));
        };
        receipt
            .claim
            .as_ref()
            .ok_or_else(|| invalid("claim receipt unavailable"))?;
        Ok(receipt.request.ship)
    }
}
