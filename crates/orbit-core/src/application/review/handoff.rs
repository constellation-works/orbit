//! Owner-domain handoff seam. Public distributed mutation tools remain gated.
//! Trusted callers obtain observations from Git/provider state and repository check
//! policy, including the existing already-landed verifier for no-diff work. They
//! must never manufacture observations by copying the worker's handoff payload.
use orbit_common::OrbitError;
use orbit_store::contracts::{
    ClaimInvocation, ClaimMutation, ClaimMutationResult, HandoffObservation,
};
use orbit_types::workflow::handoff::{HandoffCandidate, LandingStartRequest, TaskHandoff};

use crate::OrbitRuntime;
use crate::application::landing::dispatch_recorded_authority;

impl OrbitRuntime {
    pub fn accepted_task_handoff(
        &self,
        claim_id: &str,
    ) -> Result<orbit_types::workflow::handoff::AcceptedHandoff, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        self.stores().tasks().accepted_handoff(claim_id)
    }

    /// Accepting a completion-authorized handoff records its landing-start
    /// request; the owner landing job is dispatched from that request here, so
    /// no drain or ship sweep has to be running for authorized work to land.
    /// A review-only handoff records no request and dispatches nothing.
    pub fn accept_task_handoff(
        &self,
        context: &ClaimInvocation,
        request_id: &str,
        handoff: TaskHandoff,
        observation: HandoffObservation,
    ) -> Result<ClaimMutationResult, OrbitError> {
        let context = context.clone().with_handoff_observation(observation);
        let result = self.mutate_execution_claim(
            Some(&context),
            request_id,
            &ClaimMutation::AcceptHandoff(handoff),
        )?;
        dispatch_recorded_authority(self);
        Ok(result)
    }

    /// Explicit review-state approval. Does not reuse the backlog grant validator.
    pub fn approve_task_handoff(
        &self,
        context: &ClaimInvocation,
        request_id: &str,
        handoff_id: String,
        candidate: HandoffCandidate,
        observation: HandoffObservation,
    ) -> Result<ClaimMutationResult, OrbitError> {
        let context = context.clone().with_handoff_observation(observation);
        let result = self.mutate_execution_claim(
            Some(&context),
            request_id,
            &ClaimMutation::ApproveHandoff {
                handoff_id,
                candidate,
            },
        )?;
        dispatch_recorded_authority(self);
        Ok(result)
    }

    pub fn revoke_task_handoff(
        &self,
        context: &ClaimInvocation,
        request_id: &str,
        handoff_id: String,
        reason: String,
    ) -> Result<ClaimMutationResult, OrbitError> {
        self.mutate_execution_claim(
            Some(context),
            request_id,
            &ClaimMutation::RevokeHandoff { handoff_id, reason },
        )
    }

    /// The durable outbox, readable without a live drain or ship sweep. Dispatch
    /// and external merge reconciliation belong to
    /// [`crate::application::landing`].
    pub fn landing_start_requests(&self) -> Result<Vec<LandingStartRequest>, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        self.stores().tasks().landing_start_requests()
    }
}
