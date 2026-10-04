//! The runtime's side of the engine's job-level final recovery hook
//! [ORB-13907].
//!
//! Admission records the hook in the run's state before the activity runs,
//! which is what makes it once per run across crashes and resumes. A decision
//! is then recorded in the same place and, unless it is `resume`, applied:
//! through the deterministic applier for a task this workspace owns, or left
//! for the claim settlement when the run is a claimed leaf, whose task lives
//! on its owner.

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_engine::{
    FinalRecoveryAdmission, FinalRecoveryAdmissionRequest, FinalRecoveryApplication,
    FinalRecoveryApplied,
};
use orbit_types::workflow::{
    FinalRecoveryCheckpoint, FinalRecoveryDecision, FinalRecoveryObservedTask, JobRunState,
    RunStateUpdate,
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
    let applied = if matches!(decision, FinalRecoveryDecision::Resume { .. }) {
        FinalRecoveryApplied::Resume
    } else if runtime.claimed_leaf_admission(run_id)?.is_some() {
        // The owner applies it when the leaf's failure settlement arrives.
        let outcome = "recorded for the claim settlement; the owner applies it".to_string();
        match decision {
            FinalRecoveryDecision::Escalate { .. } => FinalRecoveryApplied::Escalated { outcome },
            _ => FinalRecoveryApplied::Settled { outcome },
        }
    } else {
        let observed = checkpoint.observed.ok_or_else(|| {
            OrbitError::Execution(format!(
                "run '{run_id}' admitted final recovery without observing task '{}'",
                application.task_id
            ))
        })?;
        let request = FinalRecoveryRequest {
            task_id: application.task_id.clone(),
            run_id: run_id.to_string(),
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
        applied_from(runtime.apply_final_recovery(&request, Some(&output))?)
    };
    record_decision(runtime, run_id, application, &applied)?;
    Ok(applied)
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

/// Record the decision and its outcome; a `resume` also drops the step
/// checkpoints it makes stale, so an interrupted rerun resumes from there.
fn record_decision(
    runtime: &OrbitRuntime,
    run_id: &str,
    application: &FinalRecoveryApplication,
    applied: &FinalRecoveryApplied,
) -> Result<(), OrbitError> {
    let outcome = match applied {
        FinalRecoveryApplied::Resume => "resume".to_string(),
        FinalRecoveryApplied::Settled { outcome } => format!("settled: {outcome}"),
        FinalRecoveryApplied::Escalated { outcome } => format!("escalated: {outcome}"),
    };
    runtime
        .stores()
        .jobs()
        .update_run_state(run_id, &mut |_, state| {
            if let Some(checkpoint) = state.final_recovery.as_mut() {
                checkpoint.decision = Some(application.decision.clone());
                checkpoint.outcome = Some(outcome.clone());
            }
            if let Some(from) = application.resume_step_index {
                state.step_states.retain(|index, _| *index < from);
                state.step_outputs.retain(|index, _| *index < from);
                state.compound_outputs.retain(|index, _| *index < from);
            }
            state.updated_at = Utc::now();
            Ok(())
        })
        .map(|_| ())
}

fn skipped(reason: &str) -> FinalRecoveryAdmission {
    FinalRecoveryAdmission::Skipped {
        reason: reason.to_string(),
    }
}
