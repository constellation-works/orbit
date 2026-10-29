//! One task's assessment checked, completed with the host's findings, admitted
//! under the apply's promotion authority and written atomically.
//!
//! Each phase returns the task's outcome as its error, so the first refusal
//! settles the task and its siblings still apply.

use orbit_common::OrbitError;
use orbit_types::task::{ContextCreationState, Task, TaskComplexity, TaskStatus};
use serde_json::{Value, json};

use crate::application::task::{
    CiSweepAdmission, InvalidFinding, MachineRequirement, NativeOsRequirement, REVIEW_FINDING_TAGS,
    TaskSweepFilingPort, VerdictConfidence, VerdictSource, VerdictWrite, is_review_finding,
};

use super::super::PostDeployRequirement;

use super::super::attachment_budget::{
    CONTEXT_ATTACHMENT_WARNINGS, over_attachment_findings, resolve_applied_complexity,
};
use super::super::drain_promotion;
use super::super::explore_brief::{BriefScope, decide_explore_brief, take_explore_brief};
use super::super::no_diff::VerifiedNoDiff;
use super::super::persist::{
    ApplyTaskOutcome, apply_task, operation_was_applied, record_applied_assessment,
    source_superseded, stale_task, superseded_task, task_operation_id, task_outcome,
};
use super::super::validation_tools::{self, ImplementationLane};
use super::super::{
    CONTEXT_CREATION_RETAINED, CONTEXT_EVIDENCE_RETAINED, CONTEXT_REAUTHORIZATION_REQUIRED,
    FINDING_INVALID, PILOT_NORMALIZATIONS, VALIDATION_TOOL_WARNINGS, normalize_evidence_gaps,
    post_deploy, required_machine, required_os, required_string, required_string_array,
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
    /// Creation targets the task's durable grant names.
    authorized_creation: Vec<String>,
}

/// The assessment as apply records it, with the host's findings attached.
struct Assessed {
    assessment: Value,
    after: Vec<String>,
    complexity: TaskComplexity,
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
    // The brief is the artifact's, never the receipt's: it is removed before
    // the assessment is validated, fingerprinted or audited.
    let mut assessment = assessment.clone();
    let brief = take_explore_brief(&mut assessment);
    let scope = scope(context, task_id, &mut assessment, snapshot)?;
    let authorized_creation = scope.authorized_creation.clone();
    let assessed = assess(context, task_id, &assessment, snapshot, scope)?;
    // Decided only once the assessment itself is valid, so an invalid brief
    // never makes the task `invalid` or spends the repair pass.
    let explore_brief = decide_explore_brief(
        context.action,
        brief,
        &BriefScope {
            requested: snapshot.explore_brief_requested,
            disposition: assessed.assessment["disposition"]
                .as_str()
                .unwrap_or_default(),
            recommended_complexity: assessed.assessment["recommended_complexity"].as_str(),
            after: &assessed.after,
            authorized_creation: &authorized_creation,
            workspace_root: context.workspace_root,
            source: context.source,
            source_tree: context.source_tree,
        },
    );
    let operation_id = task_operation_id(context.prepared, task_id, &assessed.assessment);
    let validated = ValidatedTask {
        task_id: task_id.clone(),
        after: assessed.after,
        assessment: assessed.assessment,
        admission: None,
        promote: false,
        history_decision: None,
        complexity: assessed.complexity,
        operation_id,
        required_os: assessed.required_os,
        required_machine: assessed.required_machine,
        post_deploy: assessed.post_deploy,
        user_facing_ui: assessed.user_facing_ui,
        applied_material: None,
        pilot_run_id: context.run_id.map(str::to_string),
        pilot_crew: context.pilot_crew.map(str::to_string),
        explore_brief,
    };
    write(context, applied, validated, snapshot)
}

fn scope(
    context: &ApplyContext<'_>,
    task_id: &str,
    assessment: &mut Value,
    snapshot: &PreparedTaskSnapshot,
) -> Settled<Scope> {
    let action = context.action;
    let mut disposition = required_string(assessment, "disposition", action)
        .map_err(refused(task_id, "invalid"))?
        .to_string();
    let proposed_after = required_string_array(assessment, "context_files_after", action)
        .map_err(refused(task_id, "invalid"))?;
    // The grant is read from the task, never from the prepared payload or the
    // agent's result. The write boundary refuses a grant that moved since
    // preparation, after its replay receipt check, so a retried apply still
    // settles as already applied.
    let (current, creation) = match context.runtime.get_task(task_id).and_then(|task| {
        let creation = context.runtime.context_creation_state(&task)?;
        Ok((Some(task), creation))
    }) {
        Ok(creation) => creation,
        Err(OrbitError::NotFound { .. }) => (None, ContextCreationState::Absent),
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
    let mut selectors = validate_after_selectors(
        action,
        task_id,
        &disposition,
        assessment,
        &proposed_after,
        context.workspace_root,
        context.source,
        authorized_creation,
    )
    .map_err(refused(task_id, "invalid"))?;
    if !authorized_creation.is_empty()
        && (disposition == "verified_no_diff"
            || (disposition == "selectors"
                && assessment
                    .get("already_landed")
                    .is_some_and(|value| !value.is_null())))
    {
        let task = current.as_ref().ok_or_else(|| {
            task_outcome(
                task_id,
                "invalid",
                Some("creation task no longer exists".into()),
            )
        })?;
        selectors.values = landed_creation_scope(
            context,
            task,
            assessment,
            &selectors.values,
            authorized_creation,
        )?;
        disposition = "selectors".into();
    }
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
        authorized_creation: authorized_creation.to_vec(),
    })
}

/// A no-change finding cannot revoke a creation grant. Prove the complete
/// retained scope at the inspection revision, then preserve it as an existing
/// already-landed hold for an operator to reconcile.
fn landed_creation_scope(
    context: &ApplyContext<'_>,
    task: &Task,
    assessment: &mut Value,
    proposed_after: &[String],
    authorized_creation: &[String],
) -> Settled<Vec<String>> {
    if let Some(finding) = assessment
        .get("already_landed")
        .filter(|value| !value.is_null())
    {
        required_string(finding, "evidence", context.action)
            .map_err(refused(task.id.as_str(), "invalid"))?;
    }
    let invalid = refused(task.id.as_str(), "invalid");
    let source = context.source.ok_or_else(|| {
        task_outcome(
            task.id.as_str(),
            "invalid",
            Some("already-landed creation work requires a pinned source revision".into()),
        )
    })?;
    let mut paths = task.context_files.clone();
    for selector in proposed_after.iter().chain(authorized_creation) {
        if !paths.contains(selector) {
            paths.push(selector.clone());
        }
    }
    // No missing-target exception: a grant authorizes future creation, but
    // does not establish that the target already exists at this revision.
    let selectors = validate_after_selectors(
        context.action,
        task.id.as_str(),
        "selectors",
        assessment,
        &paths,
        context.workspace_root,
        Some(source),
        &[],
    )
    .map_err(invalid)?;
    let finding = VerifiedNoDiff::from_landed_evidence(assessment);
    if finding.evidence.is_empty() {
        return Err(task_outcome(
            task.id.as_str(),
            "invalid",
            Some("already-landed creation work requires evidence".into()),
        ));
    }
    let covering = finding
        .creation_covering_proof(
            context.runtime,
            task,
            &selectors.values,
            &source.source_revision,
        )
        .map_err(refused(task.id.as_str(), "invalid"))?;
    let mut already_landed = assessment
        .get("already_landed")
        .filter(|value| !value.is_null())
        .cloned()
        .unwrap_or_else(|| json!({}));
    already_landed["evidence"] = json!(finding.evidence);
    already_landed["covering_commits"] = json!(covering);
    already_landed["source_revision"] = json!(source.source_revision);
    already_landed["operator_reconciliation_required"] = json!(true);
    assessment["already_landed"] = already_landed;
    assessment["disposition"] = json!("selectors");
    assessment["context_files_after"] = json!(selectors.values);
    Ok(selectors.values)
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
    invalid_finding_target(context, task_id, &assessment, &current)?;
    let criteria = current.acceptance_criteria.len();
    let native_os = required_os(
        action,
        task_id,
        &assessment,
        &current.acceptance_criteria,
        context.workspace_root,
        context.source,
    )
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
        required_os: native_os,
        required_machine: native_machine,
        post_deploy,
        user_facing_ui,
    })
}

/// A `finding_invalid` result judges a review-filed bug at the pinned
/// revision, so on any other task, or without a pinned revision, the whole
/// assessment is invalid and nothing is written. Its evidence was already
/// required to be non-empty by `validate_recommendations`.
fn invalid_finding_target(
    context: &ApplyContext<'_>,
    task_id: &str,
    assessment: &Value,
    current: &Task,
) -> Settled<()> {
    if assessment.get(FINDING_INVALID).is_none_or(Value::is_null) {
        return Ok(());
    }
    let refusal = if !is_review_finding(current) {
        format!(
            "task {task_id} {FINDING_INVALID} applies only to a review-filed bug (type bug \
             tagged {}); return null for any other task",
            REVIEW_FINDING_TAGS.join(", ")
        )
    } else if context.source.is_none() {
        format!(
            "task {task_id} {FINDING_INVALID} needs a pinned source revision to cite; a non-Git \
             workspace returns null"
        )
    } else {
        return Ok(());
    };
    Err(task_outcome(task_id, "invalid", Some(refusal)))
}

/// Record the pilot's `finding_invalid` as a REJECTED finding verdict once
/// its assessment is written, which moves the task to `rejected`. The
/// outcome is reported on the assessment as `finding_rejection`; the pilot
/// write already stands, so a rejection that does not record is reported
/// rather than failing the apply.
fn reject_invalid_finding(context: &ApplyContext<'_>, validated: &mut ValidatedTask) {
    let Some(evidence) = validated
        .assessment
        .get(FINDING_INVALID)
        .and_then(|finding| finding.get("evidence"))
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        return;
    };
    let Some(source) = context.source else {
        return;
    };
    let confidence = match validated.assessment["confidence"].as_str() {
        Some("high") => VerdictConfidence::High,
        Some("low") => VerdictConfidence::Low,
        _ => VerdictConfidence::Medium,
    };
    let finding = InvalidFinding {
        source: VerdictSource::TaskPilot,
        verified_sha: &source.source_revision,
        base_branch: &source.base_branch,
        confidence,
        detail: &evidence,
        evidence: Vec::new(),
        crew: validated.pilot_crew.as_deref(),
        run_id: validated.pilot_run_id.as_deref(),
    };
    let report = match context
        .runtime
        .reject_invalid_finding(&validated.task_id, &finding)
    {
        Ok(VerdictWrite::Recorded(status)) => json!({ "recorded": true, "status": status }),
        Ok(VerdictWrite::Superseded { reason, detail }) => {
            json!({ "recorded": false, "reason": reason, "detail": detail })
        }
        Err(error) => {
            tracing::warn!(
                target: "orbit.core.task_pilot",
                task_id = %validated.task_id,
                %error,
                "could not reject a review finding task-pilot judged invalid"
            );
            json!({ "recorded": false, "reason": "apply_failed", "detail": error.to_string() })
        }
    };
    if let Value::Object(fields) = &mut validated.assessment {
        fields.insert("finding_rejection".to_string(), report);
    }
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
    let is_done = |id: &str| graph.is_done(id);
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
            context.action,
            task_id,
            snapshot,
            current,
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
        reject_invalid_finding(context, &mut validated);
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
