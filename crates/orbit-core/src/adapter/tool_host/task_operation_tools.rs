//! Handlers for the task verbs declared in the task operation registry.
//!
//! These take the input the spec already parsed. The write verbs' handlers
//! live in [`super::task_write_tools`], artifact reads in
//! [`super::task_tools`], and the review verbs in `application::review`.

use orbit_common::OrbitError;
use orbit_common::governance::operation::{OperationInput, OperationVerb};
use orbit_common::governance::task::TaskVerb;
use orbit_types::task::{
    ExternalRef, TASK_SHOW_DELIVERY_FIELD, Task, TaskStatus, is_task_show_projection_field,
    task_base_json, unknown_task_show_field_message,
};
use serde_json::{Map, Value, json};

use crate::OrbitRuntime;
use crate::application::task::{TaskEligibilityQuery, TaskListFilter, TaskListQuery};

use super::dispatch::ToolCaller;
use super::input::{parse_task_priority, parse_task_status, parse_task_type};
use super::json::{
    serialize_error, serialize_task_lint_report, serialize_task_show,
    serialize_task_write_response, task_fields_to_json,
};
use super::task_tools::{TaskWriteOutput, write_response_fields};

/// Run one migrated task verb. A write also answers the id it persisted, for
/// the redaction audit.
pub(super) fn dispatch(
    runtime: &OrbitRuntime,
    caller: ToolCaller<'_>,
    operator: bool,
    verb: TaskVerb,
    input: Value,
) -> Result<(Value, Option<String>), OrbitError> {
    // The attachment surface has already authenticated/prepared its path-free
    // payload. Its public source_path parameter is no longer present here.
    if verb == TaskVerb::ArtifactPut {
        let output = super::task_write_tools::artifact_put(runtime, caller, &input)?;
        return Ok((output.response, Some(output.persisted_id)));
    }
    // Re-wraps the input the tool entry already parsed; see `parse_input`.
    let input = verb.spec().parse_input(input)?;
    let read = |value: Result<Value, OrbitError>| value.map(|value| (value, None));
    let written = |output: Result<TaskWriteOutput, OrbitError>| {
        output.map(|output| (output.response, Some(output.persisted_id)))
    };
    match verb {
        TaskVerb::Add => written(super::task_write_tools::add(
            runtime,
            caller,
            input.as_value(),
        )),
        TaskVerb::Update => written(super::task_write_tools::update(
            runtime,
            caller,
            input.as_value(),
            operator,
        )),
        TaskVerb::ArtifactPut => Err(OrbitError::Execution(
            "artifact preparation dispatch invariant violated".into(),
        )),
        TaskVerb::ArtifactGet => read(super::task_tools::artifact_get(runtime, &input)),
        TaskVerb::LocksList => read(crate::runtime::task::locks::list(runtime)),
        TaskVerb::LocksReserve => read(crate::runtime::task::locks::reserve(
            runtime,
            input.into_value(),
            caller.agent,
            caller.model,
            caller.reservation_owner,
        )),
        TaskVerb::LocksRelease => read(crate::runtime::task::locks::release(
            runtime,
            input.into_value(),
            caller.agent,
            caller.model,
        )),
        TaskVerb::List => read(list(runtime, &input)),
        TaskVerb::Eligible => read(eligible(runtime, &input)),
        TaskVerb::Show => read(show(runtime, &input)),
        TaskVerb::Lint => read(lint(runtime, &input)),
        TaskVerb::ReviewReset => reviewed(crate::application::review::reset_review(
            runtime,
            input.as_value(),
        )),
        TaskVerb::ReconcileReview => reviewed(
            crate::application::review::reconciliation::reconcile_review(
                runtime,
                caller.session_context,
                input.as_value(),
            ),
        ),
        TaskVerb::Reject => written(reject(runtime, &input, caller.agent, caller.model)),
        TaskVerb::Delete => read(delete(runtime, &input)),
    }
}

/// A review verb's answer, and the task it names, for the redaction audit.
fn reviewed(value: Result<Value, OrbitError>) -> Result<(Value, Option<String>), OrbitError> {
    let value = value?;
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    Ok((value, id))
}

/// `orbit.task.reject`.
fn reject(
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
        response: serialize_task_write_response(runtime, &task, response_fields.as_deref())?,
        persisted_id: task.id,
    })
}

/// `orbit.task.delete`: permanent, and outside proposed or rejected status
/// only with `force`.
fn delete(runtime: &OrbitRuntime, input: &OperationInput) -> Result<Value, OrbitError> {
    let id = input.required_string("id")?;
    let force = input.optional_bool("force")?.unwrap_or(false);
    runtime.delete_task_guarded(&id, force)?;
    Ok(json!({ "id": id, "deleted": true }))
}

fn statuses(input: &OperationInput) -> Result<Vec<TaskStatus>, OrbitError> {
    input
        .optional_string_list("status")?
        .unwrap_or_default()
        .iter()
        .map(|value| parse_task_status("status", value))
        .collect()
}

/// `orbit.task.list`. An empty page also reports how many on-disk task
/// bundles the index is missing, so a caller can tell "no tasks" from "an
/// index that needs `orbit task reindex`".
fn list(runtime: &OrbitRuntime, input: &OperationInput) -> Result<Value, OrbitError> {
    let statuses = Some(statuses(input)?).filter(|values| !values.is_empty());
    let priority = input
        .optional_string("priority")?
        .map(|value| parse_task_priority("priority", &value))
        .transpose()?;
    let task_type = input
        .optional_string("type")?
        .map(|value| parse_task_type("type", &value))
        .transpose()?;
    let external_ref = input
        .optional_string("external_ref")?
        .map(|value| ExternalRef::parse_key(&value))
        .transpose()?;
    let has_external_ref_system = input
        .optional_string("has_ref")?
        .map(|system| ExternalRef::validate_system(&system))
        .transpose()?;
    // Validated before the query so an unknown field is refused even when no
    // task matches, instead of only surfacing on the first listed task.
    let fields = projection_fields(input)?.filter(|fields| !fields.is_empty());
    let page = runtime.query_task_rows_status_aware(&TaskListQuery {
        filter: TaskListFilter {
            statuses,
            priority,
            task_type,
            parent_id: input.optional_string("parent_id")?,
            job_run_id: input.optional_string("job_run_id")?,
            tags: input.optional_string_list("tag")?.unwrap_or_default(),
            external_ref,
            has_external_ref_system,
            ..Default::default()
        },
        ready: input.optional_bool("ready")? == Some(true),
        path: input.optional_string("path")?,
        limit: super::input::task_list_limit(input.as_value())?,
    })?;
    let status_by_id = page.status_by_id;
    let tasks = page
        .items
        .into_iter()
        .map(|row| match &fields {
            None => Ok(Value::Object(task_base_json(&row.task, &status_by_id))),
            Some(fields) => project_listed_task(runtime, &row.task, fields),
        })
        .collect::<Result<Vec<_>, OrbitError>>()?;
    let total = page.total;
    let truncated = tasks.len() < total;
    let empty = tasks.is_empty();
    let mut payload = json!({
        "tasks": tasks,
        "total": total,
        "truncated": truncated,
    });
    if empty {
        let unindexed = runtime.unindexed_task_bundle_count()?;
        if unindexed > 0 {
            payload["unindexed_bundle_count"] = json!(unindexed);
        }
    }
    Ok(payload)
}

/// One listed task reduced to the requested fields, always as an object keyed by
/// field name (`orbit.task.show` returns a bare value for a single field, which
/// would make the list's element shape depend on how many fields were asked for).
fn project_listed_task(
    runtime: &OrbitRuntime,
    task: &Task,
    fields: &[String],
) -> Result<Value, OrbitError> {
    let mut projected = Map::new();
    for field in fields {
        projected.insert(
            field.clone(),
            task_fields_to_json(runtime, task, std::slice::from_ref(field))?,
        );
    }
    Ok(Value::Object(projected))
}

/// The requested `fields`, or the single-field alias `field`, each checked
/// against the show vocabulary.
fn projection_fields(input: &OperationInput) -> Result<Option<Vec<String>>, OrbitError> {
    let fields = match input.optional_string_list("fields")? {
        Some(fields) => Some(fields),
        None => input.optional_string_list("field")?,
    };
    let Some(fields) = fields else {
        return Ok(None);
    };
    let mut normalized = Vec::with_capacity(fields.len());
    for field in fields {
        let field = field.trim();
        if field.is_empty() {
            return Err(OrbitError::InvalidInput(
                "task show field selectors must not be empty".to_string(),
            ));
        }
        if field != TASK_SHOW_DELIVERY_FIELD && !is_task_show_projection_field(field) {
            return Err(OrbitError::InvalidInput(unknown_task_show_field_message(
                field,
            )));
        }
        normalized.push(field.to_string());
    }
    Ok(Some(normalized))
}

/// `orbit.task.eligible`: candidates whose lock surface overlaps no
/// `in-progress` / `review` task's surface. `conflicting` is present only with
/// `explain`.
fn eligible(runtime: &OrbitRuntime, input: &OperationInput) -> Result<Value, OrbitError> {
    let eligibility = runtime.task_eligibility(&TaskEligibilityQuery {
        statuses: statuses(input)?,
        path: input.optional_string("path")?,
        limit: super::input::task_list_limit(input.as_value())?,
    })?;
    let mut payload = Map::new();
    payload.insert(
        "tasks".to_string(),
        eligibility
            .eligible
            .iter()
            .map(eligible_task_json)
            .collect(),
    );
    payload.insert("total".to_string(), json!(eligibility.total));
    payload.insert(
        "truncated".to_string(),
        json!(eligibility.eligible.len() < eligibility.total),
    );
    if input.optional_bool("explain")? == Some(true) {
        let conflicting = eligibility
            .conflicting
            .iter()
            .map(|conflict| {
                let mut row = eligible_task_json(&conflict.task);
                row["conflicts"] = conflict
                    .overlaps
                    .iter()
                    .map(|overlap| {
                        json!({
                            "requested_file": overlap.requested_file,
                            "locking_task_id": overlap.locking_task_id,
                        })
                    })
                    .collect();
                row
            })
            .collect();
        payload.insert("conflicting".to_string(), conflicting);
    }
    Ok(Value::Object(payload))
}

fn eligible_task_json(task: &Task) -> Value {
    json!({
        "id": task.id,
        "title": task.title,
        "type": task.task_type.to_string(),
        "status": task.status.to_string(),
        "priority": task.priority.to_string(),
        "complexity": task.complexity.map(|value| value.to_string()),
    })
}

/// `orbit.task.show`: the full record, a field projection, or the delivery
/// observation.
fn show(runtime: &OrbitRuntime, input: &OperationInput) -> Result<Value, OrbitError> {
    let id = input.required_string("id")?;
    let fields = projection_fields(input)?;
    if fields
        .as_ref()
        .is_some_and(|fields| fields.iter().any(|field| field == TASK_SHOW_DELIVERY_FIELD))
    {
        return delivery(runtime, &id, fields.as_deref().unwrap_or_default(), input);
    }
    if input.as_value().get("run_id").is_some() {
        return Err(OrbitError::InvalidInput(format!(
            "`run_id` selects a delivery run and requires `field: \"{TASK_SHOW_DELIVERY_FIELD}\"`"
        )));
    }
    let task = runtime.get_task(&id)?;
    match fields {
        Some(fields) => {
            let projected = task_fields_to_json(runtime, &task, &fields)?;
            Ok(if fields.as_slice() == ["terminal"] {
                json!({ "terminal": projected })
            } else {
                projected
            })
        }
        None => serialize_task_show(runtime, &task),
    }
}

/// [ORB-13744] The bounded public delivery observation: typed host evidence
/// only, so it carries nothing to redact and nothing an agent wrote. It is a
/// response of its own rather than a task field, so it cannot be mixed with
/// record projections.
fn delivery(
    runtime: &OrbitRuntime,
    id: &str,
    fields: &[String],
    input: &OperationInput,
) -> Result<Value, OrbitError> {
    if fields.len() != 1 {
        return Err(OrbitError::InvalidInput(format!(
            "`{TASK_SHOW_DELIVERY_FIELD}` cannot be combined with other fields"
        )));
    }
    let run_id = input.optional_string("run_id")?;
    let observation = runtime.observe_task_delivery(id, run_id.as_deref())?;
    serde_json::to_value(observation).map_err(serialize_error("serialize task delivery"))
}

/// `orbit.task.lint`: one task's findings, or without `id` the context repair
/// sweep. `restore_pruned` writes the recorded restorations; nothing else
/// here writes.
fn lint(runtime: &OrbitRuntime, input: &OperationInput) -> Result<Value, OrbitError> {
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
