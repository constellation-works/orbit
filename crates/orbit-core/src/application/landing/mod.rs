//! The owner's landing consumer [ORB-12499]: durable dispatch of authorized
//! handoffs and the trusted seam the landing activity settles through.
//!
//! # Why a job, and why dispatched from the outbox
//!
//! Accepting a completion-authorized handoff — or approving a review-only one —
//! records a durable landing-start request in the owner's coordination store.
//! This module is what consumes those requests: it reserves one landing attempt
//! per handoff and submits the owner-local [`LANDING_JOB`] that carries it.
//! Dispatch happens at the moment authority is recorded and again on demand, so
//! landing never waits for a drain, a ship sweep or any schedule. A request that
//! outlives its process stays pending and is picked up by the next dispatch pass,
//! which is the recovery path for a crash between the two.
//!
//! # What deduplicates work
//!
//! Handoff identity. The attempt row is keyed by handoff, the job's action key
//! is derived from handoff plus attempt number, and a handoff whose attempt has
//! already merged is refused rather than dispatched again. A live owner job is
//! left alone; a dead one is reconciled by [`OrbitRuntime::show_job_run`] before
//! this module decides anything from its state.
//!
//! # What this module never does
//!
//! It does not merge, observe a provider, or decide that a candidate landed.
//! Those belong to the landing activity (which reads real external state) and to
//! the coordination store (which rechecks the current authorization, the exact
//! candidate and the digest-pinned validation evidence inside the transaction
//! that moves the task). This module only routes between them.

use orbit_common::OrbitError;
use orbit_engine::{HandoffLandingContext, HandoffLandingStep, HandoffLandingUpdate};
use orbit_store::contracts::{ClaimInspection, ClaimInvocation, ClaimMutation, HandoffObservation};
use orbit_types::workflow::JobRunState;
use orbit_types::workflow::handoff::{
    AcceptedHandoff, LandingAttempt, LandingAttemptState, LandingStartState,
};
use serde::Serialize;
use serde_json::json;

use crate::OrbitRuntime;

#[cfg(test)]
mod tests;

/// The owner-local job that lands one authorized handoff.
pub const LANDING_JOB: &str = "task_landing_pipeline";

/// The owner identity recorded for landing decisions in task history. Landing
/// is an owner-operator action; the approval that authorized it carries the
/// approver separately and immutably.
const LANDING_ACTOR: &str = "owner-landing";

/// One handoff's landing job, as this dispatch left it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LandingDispatch {
    pub handoff_id: String,
    pub task_id: String,
    pub attempt: u32,
    pub run_id: String,
    /// False when an attempt was already carrying this handoff, so the caller
    /// can tell a fresh dispatch from a re-read of live work.
    pub submitted: bool,
}

impl OrbitRuntime {
    /// Every recorded landing attempt, for inspection.
    pub fn landing_attempts(&self) -> Result<Vec<LandingAttempt>, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        self.stores().tasks().landing_attempts()
    }

    /// Dispatch every pending landing-start request that is not already being
    /// carried by a live owner job.
    ///
    /// Called when authority is recorded and by explicit owner recovery. It is
    /// idempotent: a merged handoff is skipped, a stopped attempt waits for the
    /// deliberate retry in [`Self::land_handoff`], and a live job is left to
    /// finish.
    pub fn dispatch_landing_requests(&self) -> Result<Vec<LandingDispatch>, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        let attempts = self.stores().tasks().landing_attempts()?;
        let mut dispatched = Vec::new();
        for request in self.stores().tasks().landing_start_requests()? {
            if request.state != LandingStartState::Pending {
                continue;
            }
            let attempt = attempts
                .iter()
                .find(|attempt| attempt.handoff_id == request.handoff_id);
            match attempt {
                // Settled work, or a stop whose evidence an operator must read
                // before anything is retried.
                Some(attempt)
                    if matches!(
                        attempt.state,
                        LandingAttemptState::Merged | LandingAttemptState::Stopped
                    ) =>
                {
                    continue;
                }
                Some(attempt) if self.landing_job_is_live(attempt)? => continue,
                _ => {}
            }
            dispatched.push(self.land_handoff(&request.handoff_id)?);
        }
        Ok(dispatched)
    }

    /// Dispatch or re-dispatch one named handoff without starting a drain.
    ///
    /// This is the explicit owner operation for retrying a stopped attempt or
    /// reconciling an uncertain one: the job's first act is to reconcile any
    /// unresolved merge intent against real external state, and every candidate,
    /// evidence and authority check runs again before it would merge anything.
    ///
    /// A handoff a live owner job already carries is returned as it stands
    /// rather than dispatched twice.
    pub fn land_handoff(&self, handoff_id: &str) -> Result<LandingDispatch, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        let existing = self.landing_attempt(handoff_id)?;
        let open_new_attempt = match &existing {
            Some(attempt) if attempt.state == LandingAttemptState::Dispatched => {
                match (&attempt.job_run_id, self.landing_job_is_live(attempt)?) {
                    (Some(run_id), true) => {
                        return Ok(LandingDispatch {
                            handoff_id: handoff_id.to_string(),
                            task_id: attempt.task_id.clone(),
                            attempt: attempt.attempt,
                            run_id: run_id.clone(),
                            submitted: false,
                        });
                    }
                    // A job that died leaves the attempt open but unusable: its
                    // dispatch key already resolves to that run, so landing this
                    // handoff again needs the next attempt.
                    (Some(_), false) => true,
                    // Opened but never submitted — the crash window between the
                    // two. The same key resubmits, or resolves the run that was
                    // in fact created.
                    (None, _) => false,
                }
            }
            // A merged handoff is refused by the store; a stopped one is a
            // deliberate retry.
            Some(_) => true,
            None => true,
        };
        if open_new_attempt || existing.is_none() {
            self.open_landing_attempt(handoff_id)?;
        }
        let attempt = self
            .landing_attempt(handoff_id)?
            .ok_or_else(|| missing_attempt(handoff_id))?;
        let result = self.submit_automation_pipeline_run(
            LANDING_JOB,
            json!({ "handoff_id": handoff_id, "task_id": attempt.task_id }),
            &format!("landing:{handoff_id}:{}", attempt.attempt),
        )?;
        if attempt.job_run_id.as_deref() != Some(result.run_id.as_str()) {
            self.attach_landing_job(handoff_id, &result.run_id)?;
        }
        Ok(LandingDispatch {
            handoff_id: handoff_id.to_string(),
            task_id: attempt.task_id,
            attempt: attempt.attempt,
            run_id: result.run_id,
            submitted: true,
        })
    }

    /// Whether this attempt's owner job is still pending or running. A run that
    /// died is reconciled by the lookup, so a crashed landing does not look live
    /// forever.
    fn landing_job_is_live(&self, attempt: &LandingAttempt) -> Result<bool, OrbitError> {
        let Some(run_id) = attempt.job_run_id.as_deref() else {
            return Ok(false);
        };
        match self.show_job_run(run_id) {
            Ok(run) => Ok(matches!(
                run.state,
                JobRunState::Pending | JobRunState::Running
            )),
            Err(OrbitError::NotFound { .. }) => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn landing_attempt(&self, handoff_id: &str) -> Result<Option<LandingAttempt>, OrbitError> {
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
    fn open_landing_attempt(&self, handoff_id: &str) -> Result<(), OrbitError> {
        let seen = self
            .landing_attempt(handoff_id)?
            .map_or(0, |attempt| attempt.attempt);
        self.mutate_landing_claim(
            handoff_id,
            &format!("landing-open:{handoff_id}:{seen}"),
            None,
        )
    }

    fn attach_landing_job(&self, handoff_id: &str, run_id: &str) -> Result<(), OrbitError> {
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

    /// The owner state one landing attempt runs against.
    ///
    /// Everything here is owner-held: the accepted candidate identity and the
    /// merge intent this owner has not resolved. The activity treats none of it
    /// as evidence that anything merged.
    pub(crate) fn handoff_landing_context(
        &self,
        handoff_id: &str,
    ) -> Result<HandoffLandingContext, OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        let (claim, accepted) = self.landing_claim(handoff_id)?;
        Ok(HandoffLandingContext {
            handoff_id: accepted.handoff_id,
            task_id: accepted.handoff.task_id,
            claim_id: accepted.handoff.claim_id,
            candidate: accepted.handoff.candidate,
            unresolved_merge_intent: claim.unresolved_merge_intent,
            workspace_path: self.paths().repo_root.clone(),
        })
    }

    /// Record one landing step from the activity that observed it.
    ///
    /// The observation is accepted only when it matches the accepted candidate
    /// exactly; a changed repository, branch, head or base refuses here instead
    /// of reaching an external merge. The required validation commands are the
    /// owner's own, captured at acceptance — never anything the step supplied.
    pub(crate) fn record_handoff_landing(
        &self,
        update: &HandoffLandingUpdate,
    ) -> Result<(), OrbitError> {
        self.ensure_coordination_task_write_permitted()?;
        let (claim, accepted) = self.landing_claim(&update.handoff_id)?;
        let mut context = self.landing_invocation(&claim);
        if matches!(
            update.step,
            HandoffLandingStep::PublishIntent { .. } | HandoffLandingStep::Complete
        ) {
            context =
                context.with_handoff_observation(self.landing_observation(update, &accepted)?);
        }
        let attempt = self
            .landing_attempt(&update.handoff_id)?
            .ok_or_else(|| missing_attempt(&update.handoff_id))?
            .attempt;
        let (mutation_id, mutation) = match &update.step {
            HandoffLandingStep::PublishIntent { intent_id } => (
                format!("landing-intent:{intent_id}"),
                ClaimMutation::MergeIntent {
                    intent_id: intent_id.clone(),
                    resolved: false,
                    evidence: update.evidence.clone(),
                },
            ),
            HandoffLandingStep::ResolveIntent { intent_id, merged } => (
                format!("landing-resolve:{intent_id}:{merged}"),
                ClaimMutation::MergeIntent {
                    intent_id: intent_id.clone(),
                    resolved: true,
                    evidence: update.evidence.clone(),
                },
            ),
            HandoffLandingStep::Complete => (
                format!("landing-complete:{}:{attempt}", update.handoff_id),
                ClaimMutation::CompleteLanding {
                    handoff_id: update.handoff_id.clone(),
                    evidence: update.evidence.clone(),
                },
            ),
            HandoffLandingStep::Stop => (
                format!("landing-stop:{}:{attempt}", update.handoff_id),
                ClaimMutation::StopLanding {
                    handoff_id: update.handoff_id.clone(),
                    reason: update.evidence.clone(),
                },
            ),
        };
        self.stores()
            .tasks()
            .mutate_execution_claim(Some(&context), &mutation_id, &mutation)?;
        Ok(())
    }

    fn landing_observation(
        &self,
        update: &HandoffLandingUpdate,
        accepted: &AcceptedHandoff,
    ) -> Result<HandoffObservation, OrbitError> {
        let observed = update.observed.as_ref().ok_or_else(|| {
            OrbitError::InvalidInput(
                "landing authority decisions require an owner candidate observation".to_string(),
            )
        })?;
        if *observed != accepted.handoff.candidate {
            return Err(OrbitError::InvalidInput(format!(
                "observed candidate for handoff '{}' is not the accepted one; landing stops \
                 until a fresh validated handoff replaces it",
                update.handoff_id
            )));
        }
        Ok(HandoffObservation {
            candidate: accepted.handoff.candidate.clone(),
            required_commands: accepted.required_commands.clone(),
        })
    }

    /// Resolve a handoff to the claim that owns it. Claims are few and the
    /// handoff identity is a digest of the accepted record, so this is an exact
    /// match rather than a search over task state.
    fn landing_claim(
        &self,
        handoff_id: &str,
    ) -> Result<(ClaimInspection, AcceptedHandoff), OrbitError> {
        for claim in self.stores().tasks().resolve_execution_claims()? {
            let Ok(accepted) = self
                .stores()
                .tasks()
                .accepted_handoff(&claim.claim.claim_id)
            else {
                continue;
            };
            if accepted.handoff_id == handoff_id {
                return Ok((claim, accepted));
            }
        }
        Err(OrbitError::InvalidInput(format!(
            "no accepted handoff '{handoff_id}' is current on this owner"
        )))
    }

    /// Landing runs as the owner operator. The capability that permits it is the
    /// durable authorization recorded at approval, which the store rechecks —
    /// including its revocation state — inside every landing transaction.
    fn landing_invocation(&self, claim: &ClaimInspection) -> ClaimInvocation {
        ClaimInvocation::trusted_operator(
            claim.claim.task_id.clone(),
            claim.claim.claim_id.clone(),
            LANDING_ACTOR.to_string(),
        )
    }
}

fn missing_attempt(handoff_id: &str) -> OrbitError {
    OrbitError::InvalidInput(format!(
        "no landing attempt is recorded for handoff '{handoff_id}'"
    ))
}

/// Dispatch after authority was recorded, without failing the durable decision
/// that has already been committed.
///
/// A handoff whose landing job could not be submitted keeps its pending request;
/// the next dispatch pass — explicit recovery or the next authorized handoff —
/// picks it up, which is exactly the crash path.
pub(crate) fn dispatch_recorded_authority(runtime: &OrbitRuntime) {
    match runtime.dispatch_landing_requests() {
        Ok(dispatched) => {
            for dispatch in dispatched.iter().filter(|dispatch| dispatch.submitted) {
                tracing::info!(
                    handoff_id = %dispatch.handoff_id,
                    task_id = %dispatch.task_id,
                    run_id = %dispatch.run_id,
                    attempt = dispatch.attempt,
                    "dispatched owner landing job for an authorized handoff"
                );
            }
        }
        Err(error) => tracing::warn!(
            error = %error,
            "owner landing dispatch failed; the pending request survives for recovery"
        ),
    }
}
