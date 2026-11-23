use super::super::super::input::{
    parse_optional_task_origin, parse_task_priority, parse_task_status, parse_task_type,
};
use crate::OrbitRuntime;
use crate::application::task::{
    TaskEligibilityQuery, TaskFieldProjection, TaskListFilter, TaskListHydration, TaskListQuery,
    TaskOsReadout, TaskRow, task_listing_field_json,
};
use orbit_common::OrbitError;
use orbit_common::governance::operation::OperationInput;
use orbit_types::task::{
    ExternalRef, HostOs, TASK_SHOW_DELIVERY_FIELD, Task, TaskStatus, is_task_show_projection_field,
    task_base_json, unknown_task_show_field_message,
};
use serde_json::{Map, Value, json};

pub(super) fn statuses(input: &OperationInput) -> Result<Vec<TaskStatus>, OrbitError> {
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
pub(super) fn list(runtime: &OrbitRuntime, input: &OperationInput) -> Result<Value, OrbitError> {
    let statuses = Some(statuses(input)?).filter(|values| !values.is_empty());
    let priority = input
        .optional_string("priority")?
        .map(|value| parse_task_priority("priority", &value))
        .transpose()?;
    let task_type = input
        .optional_string("type")?
        .map(|value| parse_task_type("type", &value))
        .transpose()?;
    let origin = parse_optional_task_origin(input.as_value(), "origin")?;
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
    let fields = projection_fields(input)?.filter(|fields| !fields.fields.is_empty());
    let schedule_bound =
        |field: &str| -> Result<Option<chrono::DateTime<chrono::Utc>>, OrbitError> {
            input
                .optional_raw_string(field)?
                .map(|raw| {
                    chrono::DateTime::parse_from_rfc3339(&raw)
                .map(|at| at.with_timezone(&chrono::Utc))
                .map_err(|error| OrbitError::InvalidInput(format!(
                    "`{field}` must be an RFC 3339 instant with an offset; got `{raw}` ({error})"
                )))
                })
                .transpose()
        };
    let not_before_from = schedule_bound("not_before_from")?;
    let not_before_until = schedule_bound("not_before_until")?;
    if not_before_from
        .zip(not_before_until)
        .is_some_and(|(from, until)| from > until)
    {
        return Err(OrbitError::InvalidInput(
            "not_before_from must be at or before not_before_until".into(),
        ));
    }
    let pilot_warnings_filter = input.optional_bool("pilot_warnings")? == Some(true);
    // Compact is the transport's internal hint, decided from the same policy
    // that will project this answer to the minimal fields. It never applies
    // when the call projects rows itself (`fields`/`view`) or when the
    // pilot-warnings filter must read audit sidecars.
    let compact = input
        .as_value()
        .get(orbit_common::governance::task::TASK_LIST_COMPACT_HINT)
        == Some(&Value::Bool(true))
        && fields.is_none()
        && !pilot_warnings_filter
        && input.as_value().get("view").is_none_or(Value::is_null);
    let page = runtime.query_task_rows_status_aware(&TaskListQuery {
        filter: TaskListFilter {
            statuses,
            not_before_from,
            not_before_until,
            scheduled_after: (input.optional_bool("scheduled")? == Some(true))
                .then(chrono::Utc::now),
            priority,
            task_type,
            origin,
            parent_id: input.optional_string("parent_id")?,
            job_run_id: input.optional_string("job_run_id")?,
            tags: input.optional_string_list("tag")?.unwrap_or_default(),
            external_ref,
            has_external_ref_system,
            ..Default::default()
        },
        pilot_warnings: pilot_warnings_filter,
        ready: input.optional_bool("ready")? == Some(true),
        runs_on: input
            .optional_string("runs_on")?
            .map(|value| {
                HostOs::parse(&value).ok_or_else(|| {
                    OrbitError::InvalidInput(format!(
                        "unsupported runs_on `{value}`; expected linux, macos or windows"
                    ))
                })
            })
            .transpose()?,
        path: input.optional_string("path")?,
        limit: super::super::super::input::task_list_limit(input.as_value())?,
        hydration: if compact {
            TaskListHydration::EnvelopeOnly
        } else {
            TaskListHydration::Full
        },
    })?;
    let status_by_id = page.status_by_id;
    // A compact row keeps none of the OS or pilot-warning enrichments, so
    // neither readout is built for it: no crew/executor read and no pilot
    // receipt artifact read (which also resolves a `get_task` per warning
    // prerequisite for proposed tasks).
    let os_readout = if compact {
        None
    } else {
        Some(TaskOsReadout::new(runtime)?)
    };
    let mut warnings = Vec::new();
    let mut tasks = Vec::with_capacity(page.items.len());
    for row in page.items {
        let value = match &fields {
            None => {
                let mut object = task_base_json(&row.task, &status_by_id);
                object.insert(
                    "not_before".into(),
                    json!(
                        row.task
                            .not_before
                            .map(orbit_types::task::format_not_before)
                    ),
                );
                if let Some(os_readout) = &os_readout {
                    os_readout.insert_into(&row.task, &mut object);
                    object.insert(
                        "pilot_warnings".into(),
                        json!(runtime.task_pilot_warning_findings(
                            &row.task,
                            &row.comments,
                            &|id: &str| runtime.warning_prerequisite_is_done(id),
                        )),
                    );
                }
                Value::Object(object)
            }
            Some(fields) => {
                // A field projection is never compact, so the readout exists.
                let readout = match &os_readout {
                    Some(readout) => readout,
                    None => {
                        return Err(OrbitError::Execution(
                            "task list field projection without an OS readout".into(),
                        ));
                    }
                };
                project_listed_task(
                    runtime,
                    &row,
                    &fields.fields,
                    readout,
                    &status_by_id,
                    &mut warnings,
                )?
            }
        };
        tasks.push(value);
    }
    let total = page.total;
    let truncated = tasks.len() < total;
    let empty = tasks.is_empty();
    let mut payload = json!({
        "tasks": tasks,
        "total": total,
        "truncated": truncated,
    });
    if !warnings.is_empty() {
        payload["warnings"] = json!(warnings);
    }
    if empty {
        let unindexed = runtime.unindexed_task_bundle_count()?;
        if unindexed > 0 {
            payload["unindexed_bundle_count"] = json!(unindexed);
        }
    }
    Ok(payload)
}

/// One listed task reduced to the requested fields, always as an object keyed by
/// field name, so the list's element shape never depends on how many fields
/// were asked for.
fn project_listed_task(
    runtime: &OrbitRuntime,
    row: &TaskRow,
    fields: &[String],
    os_readout: &TaskOsReadout,
    status_by_id: &std::collections::BTreeMap<String, orbit_types::task::TaskStatus>,
    warnings: &mut Vec<String>,
) -> Result<Value, OrbitError> {
    let task = &row.task;
    let mut projected = Map::new();
    for field in fields {
        projected.insert(
            field.clone(),
            if field == "os_requirement" {
                os_readout.requirement(task)
            } else if field == "placement_wait" {
                os_readout
                    .placement_wait(task)
                    .map_or(Value::Null, Value::String)
            } else {
                task_listing_field_json(runtime, row, field, status_by_id, warnings)?
            },
        );
    }
    Ok(Value::Object(projected))
}

/// The requested `fields`, or the single-field alias `field`, each checked
/// against the show vocabulary. `fields` keeps even one field keyed by
/// name; the `field` alias answers that field's bare value.
pub(super) fn projection_fields(
    input: &OperationInput,
) -> Result<Option<TaskFieldProjection>, OrbitError> {
    let (fields, keyed_single) = match input.optional_string_list("fields")? {
        Some(fields) => (Some(fields), true),
        None => (input.optional_string_list("field")?, false),
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
    Ok(Some(TaskFieldProjection::new(normalized, keyed_single)))
}

/// `orbit.task.eligible`: candidates whose lock surface overlaps no
/// `in-progress` / `review` task's surface. `conflicting` is present only with
/// `explain`.
pub(super) fn eligible(
    runtime: &OrbitRuntime,
    input: &OperationInput,
) -> Result<Value, OrbitError> {
    let eligibility = runtime.task_eligibility(&TaskEligibilityQuery {
        statuses: statuses(input)?,
        path: input.optional_string("path")?,
        limit: super::super::super::input::task_list_limit(input.as_value())?,
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
