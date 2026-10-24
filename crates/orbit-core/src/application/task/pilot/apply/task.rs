//! One task's assessment checked, completed with the host's findings, admitted
//! under the apply's promotion authority and written atomically.
//!
//! Each phase returns the task's outcome as its error, so the first refusal
//! settles the task and its siblings still apply.

use orbit_common::OrbitError;
use orbit_types::task::{ContextCreationState, Task, TaskComplexity, TaskStatus};
use serde_json::{Value, json};

use crate::application::sweep_filing::ci_failure::admission as ci_failure_admission;
use crate::application::task::{MachineRequirement, NativeOsRequirement};

use super::super::PostDeployRequirement;

use super::super::attachment_budget::{
    CONTEXT_ATTACHMENT_WARNINGS, over_attachment_findings, resolve_applied_complexity,
};
use super::super::drain_promotion;
use super::super::persist::{
    ApplyTaskOutcome, apply_task, operation_was_applied, record_applied_assessment,
    source_superseded, stale_task, superseded_task, task_operation_id, task_outcome,
};
use super::super::validation_tools::{self, ImplementationLane};
use super::super::{
    CONTEXT_CREATION_RETAINED, CONTEXT_EVIDENCE_RETAINED, CONTEXT_REAUTHORIZATION_REQUIRED,
    PILOT_NORMALIZATIONS, VALIDATION_TOOL_WARNINGS, normalize_evidence_gaps, post_deploy,
    required_machine, required_os, required_string, required_string_array,
    retained_review_evidence, review_filed, reviewed_evidence_drops, unauthorized_missing_targets,
    user_facing_ui, validate_after_selectors, validate_recommendations,
};
use super::admission::{
    Admission, CheckedAdmission, PreparedTaskSnapshot, PromotionAuthority, ValidatedTask,
    approve_promoted, close_verified_no_diff,
};
use super::{Applied, ApplyContext};

/// A task's outcome: `Ok` once its assessment is applied, `Err` with any
/// other settlement.
type Settled<T> = Result<T, Value>;

fn refused<'a, E: ToString>(
    task_id: &'a str,
    outcome: &'static str,
) -> impl FnOnce(E) -> Value + 'a {
    move |error| task_outcome(task_id, outcome, Some(error.to_string()))
}

/// The selectors an assessment leaves the task with, and what the host kept
/// or flagged on the way.
struct Scope {
    after: Vec<String>,
    normalizations: Vec<Value>,
    /// Granted creation targets the pilot omitted.
    retained: Vec<String>,
    /// A review-filed task's evidence the pilot omitted.
    evidence: Vec<String>,
    reauthorization: Vec<String>,
}

/// The assessment as apply records it, with the host's findings attached.
struct Assessed {
    assessment: Value,
    after: Vec<String>,
    complexity: TaskComplexity,
    current: Task,
    required_os: Vec<NativeOsRequirement>,
    required_machine: Option<MachineRequirement>,
    post_deploy: Vec<PostDeployRequirement>,
    user_facing_ui: bool,
}

pub(super) fn settle_task(
    context: &ApplyContext<'_>,
    applied: &mut Applied,
    task_id: &String,
    assessment: &Value,
    snapshot: &PreparedTaskSnapshot,
) -> Settled<Value> {
    if let Some(detail) = context.source_superseded.get(task_id) {
        return Err(source_superseded(task_id, detail));
    }
    let ci_sweep = matches!(context.authority, PromotionAuthority::CiSweep(..));
    if let Some(outcome) =
        superseded_task(context.runtime, task_id, snapshot, context.policy, ci_sweep)
            .map_err(refused(task_id, "apply_failed"))?
    {
        return Err(outcome);
    }
    let scope = scope(context, task_id, assessment, snapshot)?;
    let assessed = assess(context, task_id, assessment, snapshot, scope)?;
    let operation_id = task_operation_id(context.prepared, task_id, &assessed.assessment);
    let admission = admit(context, task_id, snapshot, &assessed, &operation_id)?;
    // The CI sweep promotes inside the atomic pilot write; a drain approves
    // afterwards through the approve transition, so its history names the
    // drain.
    let promote = matches!(
        &admission,
        Some(Admission::CiSweep(decision)) if decision["decision"] == "promote"
    );
    let history_decision = match &admission {
        Some(Admission::Drain(decision)) => drain_promotion::hold_decision(decision),
        _ => None,
    };
    let validated = ValidatedTask {
        task_id: task_id.clone(),
        after: assessed.after,
        assessment: assessed.assessment,
        admission,
        promote,
        history_decision,
        complexity: assessed.complexity,
        operation_id,
        required_os: assessed.required_os,
        required_machine: assessed.required_machine,
        post_deploy: assessed.post_deploy,
        user_facing_ui: assessed.user_facing_ui,
        applied_material: None,
        pilot_run_id: context.run_id.map(str::to_string),
    };
    write(context, applied, validated, snapshot)
}

fn scope(
    context: &ApplyContext<'_>,
    task_id: &str,
    assessment: &Value,
    snapshot: &PreparedTaskSnapshot,
) -> Settled<Scope> {
    let action = context.action;
    let disposition =
        required_string(assessment, "disposition", action).map_err(refused(task_id, "invalid"))?;
    let proposed_after = required_string_array(assessment, "context_files_after", action)
        .map_err(refused(task_id, "invalid"))?;
    // The grant is read from the task, never from the prepared payload or the
    // agent's result. The write boundary refuses a grant that moved since
    // preparation, after its replay receipt check, so a retried apply still
    // settles as already applied.
    let creation = match context
        .runtime
        .get_task(task_id)
        .and_then(|task| context.runtime.context_creation_state(&task))
    {
        Ok(creation) => creation,
        Err(OrbitError::NotFound { .. }) => ContextCreationState::Absent,
        Err(error) => {
            return Err(task_outcome(
                task_id,
                "apply_failed",
                Some(format!(
                    "read task {task_id} creation authorization: {error}"
                )),
            ));
        }
    };
    let authorized_creation = creation.selectors();
    let selectors = validate_after_selectors(
        action,
        task_id,
        disposition,
        assessment,
        &proposed_after,
        context.workspace_root,
        context.source,
        authorized_creation,
    )
    .map_err(refused(task_id, "invalid"))?;
    if !authorized_creation.is_empty() && disposition != "selectors" {
        return Err(task_outcome(
            task_id,
            "invalid",
            Some(format!(
                "task {task_id} holds operator-authorized creation targets {}; a {disposition} \
                 assessment cannot drop them. Keep them as selectors; only an operator \
                 revokes them, by replacing context_files through orbit.task.update",
                authorized_creation.join(", ")
            )),
        ));
    }
    // The pilot cannot revoke operator intent by omission: every granted
    // target it left out is kept, and reported.
    let mut after = selectors.values;
    let retained = authorized_creation
        .iter()
        .filter(|selector| !after.contains(selector))
        .cloned()
        .collect::<Vec<_>>();
    after.extend(retained.iter().cloned());
    // A review-filed task's selectors are the review's evidence: the pilot
    // may drop verified read-only evidence only with an explicit audit record.
    // Unexplained omissions retain filed order and leave scope unchanged.
    let retain_evidence = disposition == "selectors" && review_filed(&snapshot.tags);
    let mut evidence = if retain_evidence {
        retained_review_evidence(
            action,
            &snapshot.context_files,
            &after,
            context.workspace_root,
            context.source,
        )
        .map_err(refused(task_id, "apply_failed"))?
    } else {
        Vec::new()
    };
    let dropped = reviewed_evidence_drops(action, assessment, &evidence)
        .map_err(refused(task_id, "invalid"))?;
    evidence.retain(|selector| !dropped.contains(selector));
    if retain_evidence {
        after = snapshot
            .context_files
            .iter()
            .filter(|selector| after.contains(selector) || evidence.contains(selector))
            .chain(
                after
                    .iter()
                    .filter(|selector| !snapshot.context_files.contains(selector)),
            )
            .cloned()
            .collect();
    }
    let reauthorization = unauthorized_missing_targets(
        action,
        &snapshot.context_files,
        &after,
        authorized_creation,
        context.workspace_root,
        context.source,
    )
    .map_err(refused(task_id, "apply_failed"))?;
    Ok(Scope {
        after,
        normalizations: selectors.normalizations,
        retained,
        evidence,
        reauthorization,
    })
}

fn assess(
    context: &ApplyContext<'_>,
    task_id: &str,
    assessment: &Value,
    snapshot: &PreparedTaskSnapshot,
    scope: Scope,
) -> Settled<Assessed> {
    let action = context.action;
    let mut assessment = assessment.clone();
    let mut pilot_normalizations = normalize_evidence_gaps(&mut assessment)
        .into_iter()
        .collect::<Vec<_>>();
    if crate::application::task::normalize_pilot_self_blockers(&mut assessment, task_id) {
        pilot_normalizations.push(json!({
            "field": "blocked_by",
            "dropped_self_reference": task_id,
            "note": format!("warning: dropped self-reference {task_id} from blocked_by"),
        }));
    }
    let complexity = validate_recommendations(action, task_id, &assessment)
        .map_err(refused(task_id, "invalid"))?;
    let complexity = resolve_applied_complexity(complexity, snapshot.complexity);
    let current = match context.runtime.get_task(task_id) {
        Ok(task) => task,
        Err(OrbitError::NotFound { .. }) => {
            return Err(stale_task(
                task_id,
                "task_deleted",
                "task no longer exists after preparation",
            ));
        }
        Err(error) => {
            return Err(task_outcome(
                task_id,
                "apply_failed",
                Some(format!("reload task {task_id}: {error}")),
            ));
        }
    };
    let criteria = current.acceptance_criteria.len();
    let native_os = required_os(action, task_id, &assessment, &current.acceptance_criteria)
        .map_err(refused(task_id, "invalid"))?;
    let native_machine = required_machine(
        action,
        task_id,
        &assessment,
        criteria,
        context.runtime.automation_execution_location(),
    )
    .map_err(refused(task_id, "invalid"))?;
    let post_deploy = post_deploy(
        action,
        task_id,
        &assessment,
        criteria,
        &native_os,
        native_machine.as_ref(),
    )
    .map_err(refused(task_id, "invalid"))?;
    let user_facing_ui =
        user_facing_ui(action, task_id, &assessment).map_err(refused(task_id, "invalid"))?;
    attach_host_findings(
        &mut assessment,
        snapshot,
        &scope,
        complexity,
        pilot_normalizations,
    );
    // The agent may still report screenshot attachment as infeasible. Drop
    // only that claim once the host knows the implement lane can attach;
    // promotion and the stored audit then agree.
    validation_tools::drop_false_artifact_warnings(
        &mut assessment,
        &ImplementationLane::resolve(context.runtime),
        &current.required_tools,
    );
    Ok(Assessed {
        assessment,
        after: scope.after,
        complexity,
        current,
        required_os: native_os,
        required_machine: native_machine,
        post_deploy,
        user_facing_ui,
    })
}

/// The pilot never sees the deterministic findings — the lane's
/// validation-tool feasibility [ORB-11980] and this boundary's
/// over-attachment budget [ORB-12228] — so apply attaches them here: every
/// downstream readiness and admission rule then reads one assessment carrying
/// both the agent's findings and the host's.
fn attach_host_findings(
    assessment: &mut Value,
    snapshot: &PreparedTaskSnapshot,
    scope: &Scope,
    complexity: TaskComplexity,
    pilot_normalizations: Vec<Value>,
) {
    let Value::Object(fields) = assessment else {
        return;
    };
    // The host owns the before snapshot. An optional legacy agent echo cannot
    // override it or change audit/replay identity.
    fields.insert(
        "context_files_before".to_string(),
        json!(snapshot.context_files),
    );
    fields.insert(
        VALIDATION_TOOL_WARNINGS.to_string(),
        json!(snapshot.validation_tool_warnings),
    );
    fields.insert("context_files_after".to_string(), json!(scope.after));
    fields.insert(
        "selector_normalizations".to_string(),
        json!(scope.normalizations),
    );
    fields.insert(
        CONTEXT_ATTACHMENT_WARNINGS.to_string(),
        json!(over_attachment_findings(complexity, &scope.after)),
    );
    // Only the host's normalization notes may enter task history.
    fields.remove(PILOT_NORMALIZATIONS);
    if !pilot_normalizations.is_empty() {
        fields.insert(
            PILOT_NORMALIZATIONS.to_string(),
            json!(pilot_normalizations),
        );
    }
    if !scope.retained.is_empty() {
        fields.insert(CONTEXT_CREATION_RETAINED.to_string(), json!(scope.retained));
    }
    if !scope.evidence.is_empty() {
        fields.insert(CONTEXT_EVIDENCE_RETAINED.to_string(), json!(scope.evidence));
    }
    if !scope.reauthorization.is_empty() {
        fields.insert(
            CONTEXT_REAUTHORIZATION_REQUIRED.to_string(),
            json!(scope.reauthorization),
        );
    }
}

/// The promotion decision the apply's authority makes for this task.
fn admit(
    context: &ApplyContext<'_>,
    task_id: &str,
    snapshot: &PreparedTaskSnapshot,
    assessed: &Assessed,
    operation_id: &str,
) -> Settled<Option<Admission>> {
    // Prerequisites this assessment will record as edges in its atomic
    // write already enforce themselves, so promotion decides against the
    // dependencies the task will carry, not only the ones it carried at
    // preparation. Missing and other-workspace IDs add no edge and are
    // absent from the done lookup, so their findings keep holding.
    let runtime = context.runtime;
    let is_done = |id: &str| {
        matches!(
            runtime.stores().tasks().get_task(id),
            Ok(Some(task)) if task.status == TaskStatus::Done
        )
    };
    let mut dependencies = assessed.current.dependencies();
    for id in crate::application::task::pilot_blocker_task_ids(&assessed.assessment) {
        if dependencies.contains(&id) {
            continue;
        }
        if let Ok(Some(target)) = runtime.stores().tasks().get_task(&id)
            && matches!(
                target.status,
                TaskStatus::Proposed
                    | TaskStatus::Backlog
                    | TaskStatus::InProgress
                    | TaskStatus::Blocked
                    | TaskStatus::Review
            )
        {
            dependencies.push(id);
        }
    }
    let admission = match context.authority {
        PromotionAuthority::CiSweep(filing, authorized) => {
            let mut admission_task = assessed.current.clone();
            if snapshot.status == TaskStatus::Proposed
                && assessed.current.status == TaskStatus::Backlog
            {
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
                        assessed.current.status,
                        "the CI-sweep task was promoted to backlog elsewhere before admission",
                    ));
                }
            }
            ci_failure_admission::assess(
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
                ci_failure_admission::AdmissionOutcome::Decision(decision) => {
                    CheckedAdmission::Apply(Some(Admission::CiSweep(decision)))
                }
                ci_failure_admission::AdmissionOutcome::Superseded(outcome) => {
                    CheckedAdmission::Superseded(outcome)
                }
            })
        }
        PromotionAuthority::Drain(drain) => drain_promotion::assess(
            context.action,
            task_id,
            snapshot,
            &assessed.current,
            &assessed.assessment,
            &assessed.after,
            assessed.complexity,
            drain,
            &dependencies,
            &is_done,
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
fn write(
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
        Ok(ApplyTaskOutcome::Superseded(outcome)) => return Err(outcome),
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
