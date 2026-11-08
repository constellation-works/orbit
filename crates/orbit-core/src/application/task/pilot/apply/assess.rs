//! Validate one assessment and attach the deterministic host findings.
use super::super::attachment_budget::{
    CONTEXT_ATTACHMENT_WARNINGS, over_attachment_findings, resolve_applied_complexity,
};
use super::super::persist::stale_task;
use super::super::validation_tools::{self, ImplementationLane};
use super::super::{
    CONTEXT_CREATION_RETAINED, CONTEXT_EVIDENCE_RETAINED, CONTEXT_REAUTHORIZATION_REQUIRED,
    PILOT_NORMALIZATIONS, VALIDATION_TOOL_WARNINGS, consequential, contract_conflicts,
    normalize_evidence_gaps, post_deploy, required_machine, required_os, user_facing_ui,
    validate_recommendations,
};
use super::admission::PreparedTaskSnapshot;
use super::task::{Assessed, Scope, Settled, refused};
use super::{ApplyContext, footprint, invalid_finding};
use orbit_types::task::{Task, TaskComplexity};
use serde_json::{Value, json};

pub(super) fn assess_with(
    context: &ApplyContext<'_>,
    task_id: &str,
    assessment: &Value,
    snapshot: &PreparedTaskSnapshot,
    scope: Scope,
    pre_current: Option<&Task>,
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
    let recommended = validate_recommendations(action, task_id, &assessment)
        .map_err(refused(task_id, "invalid"))?;
    let complexity = resolve_applied_complexity(recommended, snapshot.complexity);
    // The caller shares its pre-lock task read; the locked write re-reads
    // under the task locks, so no decision changes here.
    let Some(current) = pre_current.cloned() else {
        return Err(stale_task(
            task_id,
            "task_deleted",
            "task no longer exists after preparation",
        ));
    };
    invalid_finding::invalid_finding_target(context, task_id, &assessment, &current)?;
    let criteria = current.acceptance_criteria.len();
    contract_conflicts(action, task_id, &assessment, criteria)
        .map_err(refused(task_id, "invalid"))?;
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
    footprint::hold_unresolved_footprint(&mut assessment, recommended, complexity);
    // The description-stated subset of an undecided consequential decision.
    // The activity prompt covers the same choice when only pinned source shows it.
    consequential::attach_undecided_consequential_decisions(&mut assessment, &current.description);
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
    let lane = ImplementationLane::resolve(context.runtime);
    validation_tools::drop_false_artifact_warnings(&mut assessment, &lane, &current.required_tools);
    // An operator-only tool the pilot classified as the change's subject is
    // no validation the lane must run, so its reserved-operation finding goes.
    validation_tools::drop_subject_operator_warnings(&mut assessment, &lane, &current);
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
