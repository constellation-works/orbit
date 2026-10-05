//! The runtime's side of the engine's job-level final recovery hook
//! [ORB-13907].
//!
//! Admission records the hook in the run's state before the activity runs,
//! which is what makes it once per run across crashes and resumes. A decision
//! is then recorded in the same place and, unless it is `resume`, applied:
//! through the deterministic applier for a task this workspace owns, or left
//! for the claim settlement when the run is a claimed leaf, whose task lives
//! on its owner.
//!
//! A local settlement is crash-safe without a journal of its own. The
//! decision is recorded in run state before the task is touched. The
//! applier's write is idempotent per admitting run, and the outcome is
//! recorded last. A crash between any two steps leaves state that a retry or
//! a resumed run replays to the same outcome. When only the last write fails,
//! the task's own record of the decision shows that it was applied.

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_engine::{
    FinalRecoveryAdmission, FinalRecoveryAdmissionRequest, FinalRecoveryApplication,
    FinalRecoveryApplied,
};
use orbit_types::workflow::{
    FinalRecoveryCheckpoint, FinalRecoveryDecision, FinalRecoveryKey, FinalRecoveryObservedTask,
    JobRunState, RunStateUpdate,
};

use crate::OrbitRuntime;
use crate::application::task::{
    FinalRecoveryCompletion, FinalRecoveryOutcome, FinalRecoveryRequest, FinalRecoveryRequeueBound,
    FinalRecoveryTaskRevision,
};

impl OrbitRuntime {
    /// Admit a failed run's final recovery, recording it in the run's state so
    /// it never runs again for this run or any run resumed from it. Skips when
    /// `workflow.final_recovery_crews` is empty, the run already spent it, or
    /// the run is no longer running.
    pub(crate) fn admit_run_final_recovery(
        &self,
        run_id: &str,
        request: &FinalRecoveryAdmissionRequest,
    ) -> Result<FinalRecoveryAdmission, OrbitError> {
        admit(self, run_id, request)
    }

    /// Record an admitted final recovery's decision and act on it: `resume`
    /// is the engine's, a claimed leaf's decision waits for its failure
    /// settlement, and any other decision goes through the applier.
    pub(crate) fn apply_run_final_recovery(
        &self,
        run_id: &str,
        application: &FinalRecoveryApplication,
    ) -> Result<FinalRecoveryApplied, OrbitError> {
        apply(self, run_id, application)
    }
}

fn admit(
    runtime: &OrbitRuntime,
    run_id: &str,
    request: &FinalRecoveryAdmissionRequest,
) -> Result<FinalRecoveryAdmission, OrbitError> {
    if runtime.context.settings().final_recovery_crews().is_empty() {
        return Ok(skipped(
            "final recovery is disabled: `workflow.final_recovery_crews` is []",
        ));
    }
    // A claimed leaf's task belongs to its owner; the owner re-reads it when
    // it applies the settled decision, so nothing is observed here.
    let Some(run) = runtime.stores().jobs().get_job_run(run_id)? else {
        return Ok(skipped("the run does not exist"));
    };
    let observed = if runtime.claimed_leaf_admission(run_id)?.is_some() {
        None
    } else {
        let task = runtime.get_task(&request.task_id)?;
        Some(FinalRecoveryObservedTask {
            status: task.status,
            updated_at: task.updated_at,
        })
    };
    let mut refusal = None;
    let update = runtime
        .stores()
        .jobs()
        .update_run_state(run_id, &mut |run_state, state| {
            if run_state != JobRunState::Running {
                refusal = Some(format!("the run is {run_state}, not running"));
            } else if state.final_recovery.is_some() {
                refusal = Some(
                    "final recovery already ran for this run or the run it resumed".to_string(),
                );
            } else {
                state.final_recovery = Some(FinalRecoveryCheckpoint {
                    key: FinalRecoveryKey {
                        run_id: run_id.to_string(),
                        attempt: run.attempt,
                    },
                    failed_step_id: request.failed_step_id.clone(),
                    task_id: request.task_id.clone(),
                    observed,
                    base_ref: request.base_ref.clone(),
                    admitted_at: Utc::now(),
                    decision: None,
                    outcome: None,
                });
                state.updated_at = Utc::now();
            }
            Ok(())
        })?;
    if update != RunStateUpdate::Updated {
        return Ok(skipped(
            "the run has no persisted state to record final recovery in",
        ));
    }
    Ok(match refusal {
        Some(reason) => skipped(&reason),
        None => FinalRecoveryAdmission::Admitted,
    })
}

fn apply(
    runtime: &OrbitRuntime,
    run_id: &str,
    application: &FinalRecoveryApplication,
) -> Result<FinalRecoveryApplied, OrbitError> {
    let checkpoint = runtime
        .stores()
        .jobs()
        .read_run_state(run_id)?
        .and_then(|state| state.final_recovery)
        .filter(|checkpoint| checkpoint.task_id == application.task_id)
        .ok_or_else(|| {
            OrbitError::Execution(format!(
                "run '{run_id}' has no final recovery admitted for task '{}'",
                application.task_id
            ))
        })?;
    let decision = &application.decision;
    if matches!(decision, FinalRecoveryDecision::Resume { .. }) {
        record_decision(
            runtime,
            run_id,
            application,
            Some(&FinalRecoveryApplied::Resume),
        )?;
        return Ok(FinalRecoveryApplied::Resume);
    }
    if let Some(recorded) = checkpoint
        .decision
        .as_ref()
        .filter(|recorded| *recorded != decision)
    {
        return Err(OrbitError::Execution(format!(
            "run '{run_id}' already recorded final recovery decision `{}`; refusing `{}`",
            recorded.kind(),
            decision.kind()
        )));
    }
    // The intent is durable before the task is touched. If recording it
    // fails, the task stays as the failure left it.
    if checkpoint.decision.is_none() {
        record_decision(runtime, run_id, application, None)?;
    }

    if runtime.claimed_leaf_admission(run_id)?.is_some() {
        // The recorded decision is what the leaf's failure settlement
        // carries; the owner applies it.
        let outcome = "recorded for the claim settlement; the owner applies it".to_string();
        let applied = match decision {
            FinalRecoveryDecision::Escalate { .. } => FinalRecoveryApplied::Escalated { outcome },
            _ => FinalRecoveryApplied::Settled { outcome },
        };
        if let Err(error) = record_decision(runtime, run_id, application, Some(&applied)) {
            tracing::warn!(
                target: "orbit.core.final_recovery",
                run_id,
                error = %error,
                "claimed leaf final recovery outcome not recorded; its recorded decision still \
                 rides on the settlement",
            );
        }
        return Ok(applied);
    }

    let observed = checkpoint.observed.ok_or_else(|| {
        OrbitError::Execution(format!(
            "run '{run_id}' admitted final recovery without observing task '{}'",
            application.task_id
        ))
    })?;
    // Keyed by the admitting run, so a run resumed from this state replays
    // the same application rather than making a second one.
    let request = FinalRecoveryRequest {
        task_id: application.task_id.clone(),
        run_id: checkpoint.key.run_id.clone(),
        observed: FinalRecoveryTaskRevision {
            status: observed.status,
            updated_at: observed.updated_at,
        },
        repo_root: application.workspace_path.clone(),
        base_ref: checkpoint.base_ref.clone().unwrap_or_default(),
        completion: if application.completion_done {
            FinalRecoveryCompletion::Done
        } else {
            FinalRecoveryCompletion::Review
        },
        requeue_bound: FinalRecoveryRequeueBound::default(),
    };
    let output = serde_json::to_value(decision)
        .map_err(|error| OrbitError::Execution(format!("encode decision: {error}")))?;
    // When a write after the applier's own fails, the task's record of this
    // run's decision is the authority. Reporting an error would escalate
    // settled work into the failure path.
    let recorded = || {
        runtime
            .recorded_final_recovery(&application.task_id, &checkpoint.key.run_id)
            .ok()
            .flatten()
    };
    let outcome = match runtime.apply_final_recovery(&request, Some(&output)) {
        Ok(outcome) => outcome,
        Err(error) => {
            let Some(outcome) = recorded() else {
                return Err(error);
            };
            warn_applied_despite(run_id, application, &error);
            outcome
        }
    };
    let applied = applied_from(outcome.clone());
    if let Err(error) = record_decision(runtime, run_id, application, Some(&applied)) {
        let reflected = recorded().is_some_and(|recorded| {
            std::mem::discriminant(&recorded) == std::mem::discriminant(&outcome)
        });
        if !reflected {
            return Err(error);
        }
        warn_applied_despite(run_id, application, &error);
    }
    Ok(applied)
}

fn warn_applied_despite(run_id: &str, application: &FinalRecoveryApplication, error: &OrbitError) {
    tracing::warn!(
        target: "orbit.core.final_recovery",
        run_id,
        task_id = application.task_id.as_str(),
        error = %error,
        "final recovery bookkeeping failed after the task took the decision; the decision stands",
    );
}

/// The engine's view of what the applier did. A refusal means a human acted
/// after the failure; the run's ordinary failure path still preserves its
/// work, so it is treated like an escalation.
fn applied_from(outcome: FinalRecoveryOutcome) -> FinalRecoveryApplied {
    match outcome {
        FinalRecoveryOutcome::Resume { .. } => FinalRecoveryApplied::Resume,
        FinalRecoveryOutcome::Completed {
            status,
            evidence_commit,
        } => FinalRecoveryApplied::Settled {
            outcome: format!("completed to {status} on {evidence_commit}"),
        },
        FinalRecoveryOutcome::Rejected => FinalRecoveryApplied::Settled {
            outcome: "rejected".to_string(),
        },
        FinalRecoveryOutcome::Archived => FinalRecoveryApplied::Settled {
            outcome: "archived".to_string(),
        },
        FinalRecoveryOutcome::Requeued => FinalRecoveryApplied::Settled {
            outcome: "requeued to backlog".to_string(),
        },
        FinalRecoveryOutcome::Escalated { reason } => FinalRecoveryApplied::Escalated {
            outcome: match reason {
                Some(reason) => format!("blocked for a human: {reason}"),
                None => "blocked for a human".to_string(),
            },
        },
        FinalRecoveryOutcome::Refused { reason } => FinalRecoveryApplied::Escalated {
            outcome: format!("refused: {reason}"),
        },
    }
}

/// Record the decision, and its outcome once known (`None` records the
/// intent alone). A `resume` also drops the step checkpoints it makes stale,
/// so an interrupted rerun resumes from there.
fn record_decision(
    runtime: &OrbitRuntime,
    run_id: &str,
    application: &FinalRecoveryApplication,
    applied: Option<&FinalRecoveryApplied>,
) -> Result<(), OrbitError> {
    let outcome = applied.map(|applied| match applied {
        FinalRecoveryApplied::Resume => "resume".to_string(),
        FinalRecoveryApplied::Settled { outcome } => format!("settled: {outcome}"),
        FinalRecoveryApplied::Escalated { outcome } => format!("escalated: {outcome}"),
    });
    let update = runtime
        .stores()
        .jobs()
        .update_run_state(run_id, &mut |_, state| {
            if let Some(checkpoint) = state.final_recovery.as_mut() {
                checkpoint.decision = Some(application.decision.clone());
                checkpoint.outcome = outcome.clone();
            }
            if let Some(from) = application.resume_step_index {
                state.step_states.retain(|index, _| *index < from);
                state.step_outputs.retain(|index, _| *index < from);
                state.compound_outputs.retain(|index, _| *index < from);
            }
            state.updated_at = Utc::now();
            Ok(())
        })?;
    if update != RunStateUpdate::Updated {
        return Err(OrbitError::Execution(format!(
            "run '{run_id}' has no persisted state to record its final recovery decision in"
        )));
    }
    Ok(())
}

fn skipped(reason: &str) -> FinalRecoveryAdmission {
    FinalRecoveryAdmission::Skipped {
        reason: reason.to_string(),
    }
}
