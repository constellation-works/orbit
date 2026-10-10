//! Landing attempt bookkeeping and the claim mutations that record it.

use orbit_common::{ClaimRefusalKind, OrbitError};
use orbit_store::contracts::{ClaimInspection, ClaimInvocation, ClaimMutation};
use orbit_types::workflow::handoff::{AcceptedHandoff, LandingAttempt};

use crate::OrbitRuntime;

/// The owner identity recorded for landing decisions in task history. Landing
/// is an owner-operator action; the approval that authorized it carries the
/// approver separately and immutably.
const LANDING_ACTOR: &str = "owner-landing";

impl OrbitRuntime {
    /// Every recorded landing attempt, for inspection.
    pub fn landing_attempts(&self) -> Result<Vec<LandingAttempt>, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        self.stores().tasks().landing_attempts()
    }

    pub(super) fn landing_attempt(
        &self,
        handoff_id: &str,
    ) -> Result<Option<LandingAttempt>, OrbitError> {
        Ok(self
            .stores()
            .tasks()
            .landing_attempts()?
            .into_iter()
            .find(|attempt| attempt.handoff_id == handoff_id))
    }

    /// Open the next attempt for this handoff, before any job exists to carry
    /// it. A crash between this and the submission leaves the request pending,
    /// which the next dispatch pass resubmits under the same key.
    pub(super) fn open_landing_attempt(&self, handoff_id: &str) -> Result<(), OrbitError> {
        let seen = self
            .landing_attempt(handoff_id)?
            .map_or(0, |attempt| attempt.attempt);
        self.mutate_landing_claim(
            handoff_id,
            &format!("landing-open:{handoff_id}:{seen}"),
            None,
        )
    }

    pub(super) fn attach_landing_job(
        &self,
        handoff_id: &str,
        run_id: &str,
    ) -> Result<(), OrbitError> {
        self.mutate_landing_claim(
            handoff_id,
            &format!("landing-attach:{handoff_id}:{run_id}"),
            Some(run_id.to_string()),
        )
    }

    fn mutate_landing_claim(
        &self,
        handoff_id: &str,
        mutation_id: &str,
        job_run_id: Option<String>,
    ) -> Result<(), OrbitError> {
        let (claim, _) = self.landing_claim(handoff_id)?;
        self.stores().tasks().mutate_execution_claim(
            Some(&self.landing_invocation(&claim)),
            mutation_id,
            &ClaimMutation::DispatchLanding {
                handoff_id: handoff_id.to_string(),
                job_run_id,
            },
        )?;
        Ok(())
    }

    /// Resolve a handoff to the claim that owns it. Claims are few and the
    /// handoff identity is a digest of the accepted record, so this is an exact
    /// match rather than a search over task state.
    pub(super) fn landing_claim(
        &self,
        handoff_id: &str,
    ) -> Result<(ClaimInspection, AcceptedHandoff), OrbitError> {
        for claim in self.stores().tasks().resolve_execution_claims()? {
            let Some(accepted) = self
                .stores()
                .tasks()
                .find_accepted_handoff(&claim.claim.claim_id)?
            else {
                continue;
            };
            if accepted.handoff_id == handoff_id {
                return Ok((claim, accepted));
            }
        }
        Err(OrbitError::ClaimRefused {
            kind: ClaimRefusalKind::NotCurrent,
            message: format!("no accepted handoff '{handoff_id}' is current on this owner"),
        })
    }

    /// Landing runs as the owner operator. The capability that permits it is the
    /// durable authorization recorded at approval, which the store rechecks —
    /// including its revocation state — inside every landing transaction.
    pub(super) fn landing_invocation(&self, claim: &ClaimInspection) -> ClaimInvocation {
        ClaimInvocation::trusted_operator(
            claim.claim.task_id.clone(),
            claim.claim.claim_id.clone(),
            LANDING_ACTOR.to_string(),
        )
    }
}

pub(super) fn missing_attempt(handoff_id: &str) -> OrbitError {
    OrbitError::InvalidInput(format!(
        "no landing attempt is recorded for handoff '{handoff_id}'"
    ))
}
