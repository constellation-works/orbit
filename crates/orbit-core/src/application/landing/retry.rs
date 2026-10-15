//! Guarded operator admission for an existing authorized owner handoff.

use orbit_common::{ClaimRefusalKind, OrbitError};
use orbit_store::contracts::{ClaimInvocation, ClaimMutation, ExecutionClaimPhase};
use orbit_types::workflow::handoff::{LandingAttemptState, LandingRetryRequest};

use super::{LANDING_JOB, LandingDispatch};
use crate::OrbitRuntime;

impl OrbitRuntime {
    /// Retry a stopped, terminal-job dispatched, or uncertain landing under
    /// trusted operator authority. The store proves exact terminal owner-job
    /// evidence atomically with reservation; runtime liveness is only a refusal.
    /// `owner_workspace_id` is explicit and must identify this owner partition.
    /// Replaying a receipt returns its original run even after it settles.
    pub fn retry_handoff_as_operator(
        &self,
        owner_workspace_id: &str,
        request: &LandingRetryRequest,
        actor: &str,
    ) -> Result<LandingDispatch, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        if actor.trim().is_empty() || self.workspace_id()? != owner_workspace_id {
            return Err(OrbitError::CapabilityDenied(
                "explicit current owner workspace and operator required".into(),
            ));
        }
        let tasks = self.stores().tasks();
        let receipt = tasks.landing_retry_receipt(&request.handoff_id, &request.request_key)?;
        if let Some(receipt) = &receipt {
            if receipt.request != *request || receipt.actor != actor {
                return Err(OrbitError::InvalidInput(
                    "retry request key already records different input or actor".into(),
                ));
            }
            let key = format!("landing:{}:{}", request.handoff_id, receipt.attempt);
            if let Some(run_id) = self.stores().jobs().automation_job_for_key(&key)? {
                // Advisory replay is never new launch authority. A lost attach
                // is recovered by the normal pending outbox dispatch as well.
                let attempt = tasks
                    .landing_attempt_record(&request.handoff_id, receipt.attempt)?
                    .ok_or_else(|| super::attempts::missing_attempt(&request.handoff_id))?;
                if attempt.job_run_id.is_none()
                    && attempt.state == LandingAttemptState::Dispatched
                    && let Err(error) = self.attach_retry_job(request, &run_id)
                {
                    tracing::warn!(%error, %run_id, "retry run already recorded; attachment waits for pending outbox recovery");
                }
                return Ok(LandingDispatch {
                    handoff_id: request.handoff_id.clone(),
                    task_id: attempt.task_id,
                    attempt: receipt.attempt,
                    run_id,
                    submitted: false,
                });
            }
        }
        let (claim, accepted) = self.landing_claim(&request.handoff_id)?;
        if claim.claim.phase != ExecutionClaimPhase::HandedOff || claim.landing_invalidated {
            return Err(OrbitError::claim_refused(ClaimRefusalKind::StaleClaim));
        }
        let prior = self
            .landing_attempt(&request.handoff_id)?
            .ok_or_else(|| super::attempts::missing_attempt(&request.handoff_id))?;
        // Reconcile the prior owner-local job, never the immutable foreign leaf.
        if receipt.is_none() && self.landing_job_is_live(&prior)? {
            // A same-key caller may have reserved and attached between our
            // receipt peek and this job read. Resolve that receipt; an
            // independent request still receives the live-job refusal.
            if tasks
                .landing_retry_receipt(&request.handoff_id, &request.request_key)?
                .is_some()
            {
                return self.retry_handoff_as_operator(owner_workspace_id, request, actor);
            }
            return Err(OrbitError::InvalidInput(
                "a landing job is still live; retry refused".into(),
            ));
        }
        let observation = self.observe_claim_handoff(&accepted.handoff, false)?;
        let context = ClaimInvocation::trusted_operator(
            claim.claim.task_id.clone(),
            claim.claim.claim_id.clone(),
            actor.to_string(),
        )
        .with_handoff_observation(observation);
        if let Err(error) = tasks.mutate_execution_claim(
            Some(&context),
            &format!("landing-retry:{}", request.request_key),
            &ClaimMutation::RetryLanding(request.clone()),
        ) {
            // The winner may even finish before a same-key peer reaches its
            // reservation replay. A recorded run is an advisory outcome,
            // never permission to launch another attempt.
            if let Some(recorded) =
                tasks.landing_retry_receipt(&request.handoff_id, &request.request_key)?
                && self
                    .stores()
                    .jobs()
                    .automation_job_for_key(&format!(
                        "landing:{}:{}",
                        request.handoff_id, recorded.attempt
                    ))?
                    .is_some()
            {
                return self.retry_handoff_as_operator(owner_workspace_id, request, actor);
            }
            return Err(error);
        }
        let receipt = tasks
            .landing_retry_receipt(&request.handoff_id, &request.request_key)?
            .ok_or_else(|| OrbitError::Store("retry receipt missing after reservation".into()))?;
        // Use the reserved attempt's key, never land_handoff's read/open loop:
        // another request cannot turn this request into a later attempt.
        let result = self.submit_automation_pipeline_run(
            LANDING_JOB,
            serde_json::json!({"handoff_id": request.handoff_id, "task_id": claim.claim.task_id}),
            &format!("landing:{}:{}", request.handoff_id, receipt.attempt),
            orbit_types::workflow::JobRunTrigger::cli(),
        )?;
        self.attach_retry_job(request, &result.run_id)?;
        Ok(LandingDispatch {
            handoff_id: request.handoff_id.clone(),
            task_id: claim.claim.task_id,
            attempt: receipt.attempt,
            run_id: result.run_id,
            submitted: true,
        })
    }
    fn attach_retry_job(
        &self,
        request: &LandingRetryRequest,
        run_id: &str,
    ) -> Result<(), OrbitError> {
        let (claim, _) = self.landing_claim(&request.handoff_id)?;
        // A worker may finish before the submitter receives its reply. The
        // run key is already durable; a settled attempt needs no launch attach.
        if claim.claim.phase == ExecutionClaimPhase::Landed {
            return Ok(());
        }
        if let Some(attempt) = self.landing_attempt(&request.handoff_id)?
            && attempt.state == LandingAttemptState::Stopped
        {
            return Ok(());
        }
        self.stores().tasks().mutate_execution_claim(
            Some(&self.landing_invocation(&claim)),
            &format!("landing-retry-attach:{}:{run_id}", request.request_key),
            &ClaimMutation::AttachLandingRetry {
                handoff_id: request.handoff_id.clone(),
                request_key: request.request_key.clone(),
                job_run_id: run_id.into(),
            },
        )?;
        Ok(())
    }
}
