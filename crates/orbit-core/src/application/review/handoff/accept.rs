//! Accepting, approving and revoking a follower's task handoff.

use orbit_common::OrbitError;
use orbit_store::contracts::{
    ClaimInvocation, ClaimMutation, ClaimMutationOutcome, ClaimMutationResult, HandoffObservation,
};
use orbit_types::workflow::handoff::{HandoffCandidate, LandingStartRequest, TaskHandoff};

use crate::OrbitRuntime;
use crate::application::job::JobDrainPort;
use crate::application::review::ReviewLandingDispatchPort;

impl OrbitRuntime {
    pub fn accepted_task_handoff(
        &self,
        claim_id: &str,
    ) -> Result<orbit_types::workflow::handoff::AcceptedHandoff, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        self.stores()
            .tasks()
            .find_accepted_handoff(claim_id)?
            .ok_or_else(|| OrbitError::InvalidInput("typed handoff unavailable".into()))
    }

    /// Accepting a completion-authorized handoff records its landing-start
    /// request; the owner landing job is dispatched from that request here, so
    /// no drain has to be running for authorized work to land.
    /// A review-only handoff records no request and dispatches nothing.
    ///
    /// An accepted before-PR handoff's certificate is recorded in this
    /// owner's review store, so after-landing coverage excludes the reviewed
    /// tree as it does for the owner's own gate [ORB-13895]. A replayed
    /// acceptance records it again, which is a no-op.
    pub fn accept_task_handoff(
        &self,
        context: &ClaimInvocation,
        request_id: &str,
        handoff: TaskHandoff,
        observation: HandoffObservation,
    ) -> Result<ClaimMutationResult, OrbitError> {
        self.accept_task_handoff_outcome(context, request_id, handoff, observation)
            .map(|outcome| outcome.result)
    }

    /// [`Self::accept_task_handoff`], also reporting whether this call
    /// committed the acceptance rather than replaying its receipt.
    pub(crate) fn accept_task_handoff_outcome(
        &self,
        context: &ClaimInvocation,
        request_id: &str,
        handoff: TaskHandoff,
        observation: HandoffObservation,
    ) -> Result<ClaimMutationOutcome, OrbitError> {
        let claim_id = handoff.claim_id.clone();
        // A worker cannot owe a criterion its pilot finding did not type.
        self.check_owed_criteria(&self.get_task(&handoff.task_id)?, &handoff.owed_criteria)?;
        let context = context.clone().with_handoff_observation(observation);
        let outcome = self.mutate_execution_claim_outcome(
            Some(&context),
            request_id,
            &ClaimMutation::AcceptHandoff(handoff),
        )?;
        self.record_handoff_review_certificate(&claim_id)?;
        let accepted = self.accepted_task_handoff(&claim_id)?;
        for reference in &accepted.handoff.validation {
            let artifact = self
                .get_task_artifact(&accepted.handoff.task_id, &reference.path)?
                .ok_or_else(|| {
                    OrbitError::PolicyDenied("accepted validation artifact missing".into())
                })?;
            if orbit_common::security::release::sha256_hex(&artifact.content) != reference.sha256 {
                return Err(OrbitError::PolicyDenied(
                    "accepted validation artifact changed".into(),
                ));
            }
            let log: orbit_types::workflow::handoff::HandoffValidationLog =
                serde_json::from_slice(&artifact.content).map_err(|error| {
                    OrbitError::Execution(format!("decode accepted validation: {error}"))
                })?;
            if let Some(flaky) = &log.flaky {
                super::super::flaky::record_frictions(self, &accepted.handoff.task_id, flaky)?;
            }
        }
        self.dispatch_recorded_landing_authority();
        Ok(outcome)
    }

    /// Record the certificate an accepted before-PR handoff pinned, read
    /// back through the claim journal's digest check.
    fn record_handoff_review_certificate(&self, claim_id: &str) -> Result<(), OrbitError> {
        let Some(certificate) = self
            .owner_commit_boundary()?
            .accepted_review_certificate(claim_id)?
        else {
            return Ok(());
        };
        self.review_store()?
            .review_certificate_record(&self.workspace_id()?, &certificate)
    }

    /// Explicit review-state approval. Does not reuse the backlog grant validator.
    pub(crate) fn approve_task_handoff(
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
        self.dispatch_recorded_landing_authority();
        Ok(result)
    }

    pub(crate) fn revoke_task_handoff(
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

    /// The durable outbox, readable without a live drain. Dispatch and external
    /// merge reconciliation belong to the owner landing consumer.
    pub fn landing_start_requests(&self) -> Result<Vec<LandingStartRequest>, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        self.stores().tasks().landing_start_requests()
    }
}
