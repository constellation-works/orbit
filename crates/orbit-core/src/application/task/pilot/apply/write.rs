//! Admission under the apply authority and atomic writes of validated tasks.
use super::super::drain_promotion;
use super::super::persist::{
    ApplyTaskOutcome, apply_task, operation_was_applied, record_applied_assessment, stale_task,
    task_outcome,
};
use super::admission::{
    Admission, CheckedAdmission, PreparedTaskSnapshot, PromotionAuthority, ValidatedTask,
    approve_promoted, close_verified_no_diff,
};
use super::task::{Settled, refused};
use super::{Applied, ApplyContext, invalid_finding};
use crate::application::task::{CiSweepAdmission, TaskSweepFilingPort};
use orbit_types::task::{Task, TaskStatus};
use serde_json::Value;

/// The promotion decision the apply's authority makes for this task.
fn admit(
    context: &ApplyContext<'_>,
    snapshot: &PreparedTaskSnapshot,
    current: &Task,
    assessed: &ValidatedTask,
    graph: &super::super::blockers::PilotDependencyGraph,
) -> Settled<Option<Admission>> {
    // Only edges in the locked mutation (or already stored on replay) can
    // cover a prerequisite. The done lookup reads that same locked graph.
    let task_id = assessed.task_id.as_str();
    let operation_id = assessed.operation_id.as_str();
    let dependencies = current.dependencies();
    // The graph is local by design; a prerequisite owned by another
    // registered workspace takes its status from the registry instead
    // [ORB-21172].
    let is_done = |id: &str| {
        graph.is_done(id)
            || matches!(
                context.runtime.get_dependency_task(id),
                Ok(Some(target)) if target.status == TaskStatus::Done
            )
    };
    let admission = match context.authority {
        PromotionAuthority::CiSweep(filing, authorized) => {
            let mut admission_task = current.clone();
            if snapshot.status == TaskStatus::Proposed && current.status == TaskStatus::Backlog {
                if operation_was_applied(context.runtime, task_id, operation_id)
                    .map_err(refused(task_id, "apply_failed"))?
                {
                    // The atomic CI-sweep write itself promotes to backlog.
                    // Reconstruct admission reporting from its original status
                    // only for this operation's receipt; apply_task rechecks it
                    // under task locks.
                    admission_task.status = snapshot.status;
                } else {
                    // Another pilot, a drain or the operator already admitted
                    // the filed task: the sweep's purpose is met, so its stale
                    // assessment is dropped without failing the sweep.
                    return Err(super::super::superseded_outcome(
                        task_id,
                        "promoted_elsewhere",
                        current.status,
                        "the CI-sweep task was promoted to backlog elsewhere before admission",
                    ));
                }
            }
            context
                .runtime
                .ci_sweep_admission(
                    context.action,
                    task_id,
                    &admission_task,
                    &assessed.assessment,
                    &assessed.after,
                    filing,
                    *authorized,
                    &dependencies,
                    &is_done,
                )
                .map(|outcome| match outcome {
                    CiSweepAdmission::Decision(decision) => {
                        CheckedAdmission::Apply(Some(Admission::CiSweep(decision)))
                    }
                    CiSweepAdmission::Superseded(outcome) => CheckedAdmission::Superseded(outcome),
                })
        }
        PromotionAuthority::Drain(drain) => drain_promotion::assess_drain_admission(
            drain_promotion::DrainAdmissionTask {
                action: context.action,
                task_id,
                snapshot,
                current,
                assessment: &assessed.assessment,
                selectors: &assessed.after,
                complexity: assessed.complexity,
                dependencies: &dependencies,
                is_done: &is_done,
            },
            drain,
        )
        .map(|decision| CheckedAdmission::Apply(Some(Admission::Drain(decision)))),
        PromotionAuthority::None => Ok(CheckedAdmission::Apply(None)),
    };
    match admission.map_err(refused(task_id, "invalid"))? {
        CheckedAdmission::Apply(admission) => Ok(admission),
        CheckedAdmission::Superseded(outcome) => Err(outcome),
    }
}

/// Write the validated task atomically, then let a drain approve it and close
/// it when either authority verified it already fixed.
pub(super) fn write(
    context: &ApplyContext<'_>,
    applied: &mut Applied,
    mut validated: ValidatedTask,
    snapshot: &PreparedTaskSnapshot,
) -> Settled<Value> {
    let task_id = validated.task_id.clone();
    let outcome = apply_task(
        context.runtime,
        snapshot,
        &mut validated,
        context.prepared,
        context.policy,
        matches!(context.authority, PromotionAuthority::CiSweep(..)),
        |current, assessed, graph| admit(context, snapshot, current, assessed, graph),
    );
    let outcome = match (outcome, context.authority) {
        (
            Ok(applied @ (ApplyTaskOutcome::Applied(_) | ApplyTaskOutcome::AlreadyApplied(_))),
            PromotionAuthority::Drain(drain),
        ) => approve_promoted(
            context.runtime,
            &mut validated,
            snapshot,
            context.policy,
            drain,
        )
        .map(|()| applied),
        (outcome, _) => outcome,
    };
    if matches!(
        outcome,
        Ok(ApplyTaskOutcome::Applied(_) | ApplyTaskOutcome::AlreadyApplied(_))
    ) {
        // Rejected first: an invalid finding is never archived as fixed.
        invalid_finding::reject_invalid_finding(context, &mut validated);
        close_verified_no_diff(context.runtime, &mut validated);
    }
    let (label, fingerprint) = match outcome {
        Ok(ApplyTaskOutcome::Applied(fingerprint)) => ("applied", fingerprint),
        Ok(ApplyTaskOutcome::AlreadyApplied(fingerprint)) => ("already_applied", fingerprint),
        Ok(ApplyTaskOutcome::Stale(reason, detail)) => {
            return Err(stale_task(&task_id, reason, &detail));
        }
        Ok(ApplyTaskOutcome::Invalid(detail)) => {
            return Err(task_outcome(&task_id, "invalid", Some(detail)));
        }
        Ok(ApplyTaskOutcome::Superseded(outcome) | ApplyTaskOutcome::AdmissionRefused(outcome)) => {
            return Err(outcome);
        }
        Err(error) => return Err(refused(&task_id, "apply_failed")(error)),
    };
    if let Some(fingerprint) = fingerprint {
        applied
            .resulting_fingerprints
            .insert(task_id.clone(), fingerprint);
    }
    record_applied_assessment(
        validated,
        snapshot,
        label,
        &mut applied.task_results,
        &mut applied.ci_sweep_admission,
        &mut applied.drain_approval,
    );
    Ok(task_outcome(&task_id, label, None))
}
