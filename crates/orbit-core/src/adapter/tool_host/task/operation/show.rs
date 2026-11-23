use super::super::super::json::{serialize_task_show, task_fields_to_json_from_row};
use super::lifecycle::delivery;
use super::listing::projection_fields;
use crate::OrbitRuntime;
use crate::application::task::TaskRow;
use orbit_common::governance::operation::OperationInput;
use orbit_common::{NotFoundKind, OrbitError, RemoteToolCode};
use orbit_types::task::TASK_SHOW_DELIVERY_FIELD;
use serde_json::{Value, json};

/// `orbit.task.show`: the full record, a field projection, or the delivery
/// observation.
pub(super) fn show(runtime: &OrbitRuntime, input: &OperationInput) -> Result<Value, OrbitError> {
    let id = input.required_string("id")?;
    let fields = projection_fields(input)?;
    if fields.as_ref().is_some_and(|projection| {
        projection
            .fields
            .iter()
            .any(|field| field == TASK_SHOW_DELIVERY_FIELD)
    }) {
        return delivery(
            runtime,
            &id,
            fields
                .as_ref()
                .map_or(&[][..], |projection| projection.fields.as_slice()),
            input,
        );
    }
    if input.as_value().get("run_id").is_some() {
        return Err(OrbitError::InvalidInput(format!(
            "`run_id` selects a delivery run and requires `field: \"{TASK_SHOW_DELIVERY_FIELD}\"`"
        )));
    }
    let row = show_task_row(runtime, &id)?;
    match fields {
        Some(projection) => {
            let projected = task_fields_to_json_from_row(
                runtime,
                &row,
                &projection.fields,
                projection.keyed_single,
            )?;
            Ok(
                if projection.fields.as_slice() == ["terminal"] && !projection.keyed_single {
                    json!({ "terminal": projected })
                } else {
                    projected
                },
            )
        }
        None => serialize_task_show(runtime, &row),
    }
}

/// One show's task row.
///
/// The owner reads `get_task_row` once. A missing id returns that error with
/// no second read. Any other bundle failure falls back to one lightweight
/// row, and only when that row reports an unavailable pilot assessment.
/// A worker keeps the owner projections: the happy path assembles the row
/// from them, and a corrupt bundle asks the owner for `listed_task`.
fn show_task_row(runtime: &OrbitRuntime, id: &str) -> Result<TaskRow, OrbitError> {
    if runtime.worker_invocation().is_some() {
        return worker_show_task_row(runtime, id);
    }
    match runtime.get_task_row(id) {
        Ok(row) => Ok(row),
        Err(error) if is_show_not_found(&error) => Err(error),
        Err(error) => match runtime.get_listed_task_row(id)? {
            Some(row) if pilot_assessment_unavailable(runtime, &row) => Ok(row),
            _ => Err(error),
        },
    }
}

fn worker_show_task_row(runtime: &OrbitRuntime, id: &str) -> Result<TaskRow, OrbitError> {
    match runtime.get_task(id) {
        Ok(task) => Ok(TaskRow {
            task,
            comments: runtime.get_task_comments(id)?,
            history: runtime.get_task_history(id)?,
            artifacts: runtime.get_task_artifact_manifest(id)?,
        }),
        Err(error) if is_show_not_found(&error) => Err(error),
        Err(error) => match runtime.read_owner::<Value>(id, "listed_task") {
            Ok(Value::Null) => Err(error),
            Ok(value) => {
                let row = serde_json::from_value(value).map_err(|decode| {
                    OrbitError::Store(format!("worker listed task row: {decode}"))
                })?;
                if pilot_assessment_unavailable(runtime, &row) {
                    Ok(row)
                } else {
                    Err(error)
                }
            }
            Err(projected) if unknown_worker_read_projection(&projected) => {
                legacy_listed_show_row(runtime, id, error)
            }
            Err(_) => Err(error),
        },
    }
}

/// Owners that predate the `listed_task` projection still serve the readout
/// through the workspace list. A current owner never takes this path.
fn legacy_listed_show_row(
    runtime: &OrbitRuntime,
    id: &str,
    error: OrbitError,
) -> Result<TaskRow, OrbitError> {
    let Some(task) = runtime.list_tasks()?.into_iter().find(|task| task.id == id) else {
        return Err(error);
    };
    let comments = runtime.get_task_comments(id)?;
    let row = TaskRow {
        task,
        comments,
        history: runtime.get_task_history(id)?,
        artifacts: runtime.get_task_artifact_manifest(id)?,
    };
    if pilot_assessment_unavailable(runtime, &row) {
        Ok(row)
    } else {
        Err(error)
    }
}

fn pilot_assessment_unavailable(runtime: &OrbitRuntime, row: &TaskRow) -> bool {
    runtime
        .task_pilot_warning_findings(&row.task, &row.comments, &|_| false)
        .iter()
        .any(|finding| finding.kind == "pilot_assessment_unavailable")
}

fn is_show_not_found(error: &OrbitError) -> bool {
    matches!(
        error,
        OrbitError::NotFound {
            kind: NotFoundKind::Task,
            ..
        } | OrbitError::RemoteTool {
            code: RemoteToolCode::NotFound,
            ..
        }
    )
}

fn unknown_worker_read_projection(error: &OrbitError) -> bool {
    error.to_string().contains("unknown worker read projection")
}
