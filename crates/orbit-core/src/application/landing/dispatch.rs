//! Dispatch pending landing-start requests and re-dispatch one named handoff.

use orbit_common::OrbitError;
use orbit_types::workflow::JobRunState;
use orbit_types::workflow::handoff::{LandingAttempt, LandingAttemptState, LandingStartState};
use serde::Serialize;
use serde_json::json;

use super::LANDING_JOB;
use super::attempts::missing_attempt;
use crate::OrbitRuntime;

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
            orbit_types::workflow::JobRunTrigger::cli(),
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
