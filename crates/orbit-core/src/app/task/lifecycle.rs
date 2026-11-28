use super::ToolCaller;
use super::listing::statuses;
use super::write::{TaskWriteOutput, write_response_fields};
use crate::OrbitRuntime;
use crate::adapter::tool_host::json::{serialize_error, serialize_task_lint_report};
use crate::application::task::{
    TaskWriteMetadata, TaskWriteResponsePolicy, task_write_response_json,
};
use orbit_common::OrbitError;
use orbit_common::governance::operation::OperationInput;
use orbit_types::task::{TASK_SHOW_DELIVERY_FIELD, TaskStatus};
use serde_json::{Value, json};

/// `orbit.task.block_triage`: the pilot-warning candidate list, or one
/// triage decision on a held proposed task. Blocked tasks are decided by
/// final recovery, so the retired `block` subject is refused.
pub(super) fn block_triage(
    runtime: &OrbitRuntime,
    caller: ToolCaller<'_>,
    input: &OperationInput,
) -> Result<(Value, Option<String>), OrbitError> {
    use crate::application::task::escalation_triage_steps::TriageInspectionProof;
    use crate::application::task::{PilotWarningDecision, PilotWarningTriageRequest};

    let action = input.required_string("action")?;
    match input.optional_string("subject")?.as_deref() {
        None | Some("pilot_warning") => {}
        Some("block") => {
            return Err(OrbitError::InvalidInput(
                "blocked-task triage is retired: final recovery decides blocked tasks and opens \
                 the decision request an operator answers; only `subject: pilot_warning` remains"
                    .into(),
            ));
        }
        Some(other) => {
            return Err(OrbitError::InvalidInput(format!(
                "triage subject `{other}` is not pilot_warning"
            )));
        }
    }
    if action == "candidates" {
        return Ok((runtime.pilot_warning_candidates_json()?, None));
    }
    let id = input.required_string("id")?;
    let reason = || {
        input.optional_string("reason").and_then(|reason| {
            reason.ok_or_else(|| {
                OrbitError::InvalidInput(format!("triage `{action}` needs `reason`"))
            })
        })
    };
    let decision = match action.as_str() {
        "report" => PilotWarningDecision::Report {
            human_action: reason()?,
            summary: input.optional_string("summary")?,
        },
        "propose" => {
            let (summary, options, recommended) = proposal(input)?;
            PilotWarningDecision::Propose {
                summary,
                options,
                recommended,
            }
        }
        other => {
            return Err(OrbitError::InvalidInput(format!(
                "pilot warning triage action `{other}` is not one of candidates, propose or report"
            )));
        }
    };
    let requested_by = runtime
        .actor()
        .resolve_write_label(caller.agent.as_deref(), caller.model.as_deref())?;
    // The decision is validated in full before the inspection proof, so a
    // malformed call reports its own error and spends no `git` process.
    runtime.check_triage_inspection_proof(
        &id,
        &TriageInspectionProof {
            source_revision: input.optional_string("source_revision")?,
            observed_revision: input.optional_string("observed_revision")?,
            observed_tree: input.optional_string("observed_tree")?,
        },
    )?;
    let response = runtime.record_pilot_warning_triage(&PilotWarningTriageRequest {
        task_id: id.clone(),
        decision,
        requested_by,
    })?;
    Ok((response, Some(id)))
}

/// A triage `propose` call's `summary`, `options` and 1-based `recommended`
/// index. Options parse strictly: an unknown action or field is refused here.
fn proposal(
    input: &OperationInput,
) -> Result<(String, Vec<orbit_types::task::DecisionOptionInput>, usize), OrbitError> {
    let summary = input.optional_string("summary")?.ok_or_else(|| {
        OrbitError::InvalidInput(
            "triage `propose` needs `summary`: one or two plain sentences naming the decision"
                .into(),
        )
    })?;
    let options = input
        .as_value()
        .get("options")
        .cloned()
        .ok_or_else(|| {
            OrbitError::InvalidInput(
                "triage `propose` needs `options`: two or three {summary, consequence, rewrite, \
                 action} objects"
                    .into(),
            )
        })
        .and_then(|options| {
            serde_json::from_value::<Vec<orbit_types::task::DecisionOptionInput>>(options).map_err(
                |error| OrbitError::InvalidInput(format!("triage `propose` options: {error}")),
            )
        })?;
    let recommended = input
        .as_value()
        .get("recommended")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            OrbitError::InvalidInput(
                "triage `propose` needs `recommended`: the 1-based option index".into(),
            )
        })?;
    Ok((
        summary,
        options,
        usize::try_from(recommended).unwrap_or(usize::MAX),
    ))
}

/// `orbit.task.reject`.
pub(super) fn reject(
    runtime: &OrbitRuntime,
    input: &OperationInput,
    agent: Option<String>,
    model: Option<String>,
) -> Result<TaskWriteOutput, OrbitError> {
    let id = input.required_string("id")?;
    let note = input.required_string("note")?;
    let response_fields = write_response_fields(input.as_value())?;
    let task = runtime.reject_task_with_identity(
        &id,
        note,
        input.optional_string("comment")?,
        agent,
        model,
    )?;
    Ok(TaskWriteOutput {
        response: task_write_response_json(
            runtime,
            &task,
            &TaskWriteResponsePolicy::from_fields(response_fields.as_ref()),
            &TaskWriteMetadata::default(),
        )?,
        persisted_id: task.id,
    })
}

/// `orbit.task.delete`: permanent, and outside proposed or rejected status
/// only with `force`.
pub(super) fn delete(runtime: &OrbitRuntime, input: &OperationInput) -> Result<Value, OrbitError> {
    let id = input.required_string("id")?;
    let force = input.optional_bool("force")?.unwrap_or(false);
    runtime.delete_task_guarded(&id, force)?;
    Ok(json!({ "id": id, "deleted": true }))
}

/// [ORB-13744] The bounded public delivery observation: typed host evidence
/// only, so it carries nothing to redact and nothing an agent wrote. It is a
/// response of its own rather than a task field, so it cannot be mixed with
/// record projections.
pub(super) fn delivery(
    runtime: &OrbitRuntime,
    id: &str,
    fields: &[String],
    input: &OperationInput,
) -> Result<Value, OrbitError> {
    if fields.len() != 1 {
        return Err(OrbitError::InvalidInput(format!(
            "`{TASK_SHOW_DELIVERY_FIELD}` cannot be combined with other fields; pass `field: \"{TASK_SHOW_DELIVERY_FIELD}\"` alone. Optional `run_id` is allowed only with this selection."
        )));
    }
    let run_id = input.optional_string("run_id")?;
    let observation = runtime.observe_task_delivery(id, run_id.as_deref())?;
    serde_json::to_value(observation).map_err(serialize_error("serialize task delivery"))
}

/// `orbit.task.lint`: one task's findings, or without `id` the context repair
/// sweep. `restore_pruned` writes the recorded restorations; nothing else
/// here writes.
pub(super) fn lint(runtime: &OrbitRuntime, input: &OperationInput) -> Result<Value, OrbitError> {
    let restore = input.optional_bool("restore_pruned")? == Some(true);
    let statuses = statuses(input)?;
    let Some(id) = input.optional_string("id")? else {
        return sweep(runtime, &statuses, restore);
    };
    if !statuses.is_empty() {
        return Err(OrbitError::InvalidInput(
            "`status` narrows the sweep and cannot be combined with `id`".to_string(),
        ));
    }
    let restoration = if restore {
        Some(runtime.restore_pruned_context_files(&id)?.1)
    } else {
        None
    };
    let mut value = serialize_task_lint_report(&runtime.lint_task(&id)?)?;
    if let (Some(restoration), Some(object)) = (restoration, value.as_object_mut()) {
        object.insert("restored".to_string(), json!(restoration.restored));
        object.insert("unrestorable".to_string(), json!(restoration.unrestorable));
    }
    Ok(value)
}

fn sweep(
    runtime: &OrbitRuntime,
    statuses: &[TaskStatus],
    restore: bool,
) -> Result<Value, OrbitError> {
    let candidates = runtime.sweep_context_repairs(statuses, restore)?;
    let tasks: Vec<Value> = candidates
        .iter()
        .map(|candidate| {
            json!({
                "id": candidate.restoration.task_id,
                "status": candidate.status,
                "empty_surface": candidate.empty_surface,
                "invalid": candidate.invalid,
                "restorable": candidate.restoration.restored,
                "unrestorable": candidate.restoration.unrestorable,
                "written": candidate.restoration.applied,
            })
        })
        .collect();
    Ok(json!({
        "tasks_needing_repair": candidates.len(),
        "total_restorable": candidates
            .iter()
            .map(|candidate| candidate.restoration.restored.len())
            .sum::<usize>(),
        "total_unrestorable": candidates
            .iter()
            .map(|candidate| candidate.restoration.unrestorable.len())
            .sum::<usize>(),
        "tasks_written": candidates
            .iter()
            .filter(|candidate| candidate.restoration.applied)
            .count(),
        "dry_run": !restore,
        "tasks": tasks,
    }))
}
