use std::collections::BTreeMap;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use orbit_common::OrbitError;
use orbit_types::task::{
    ArtifactPresentation, MAX_TASK_ARTIFACT_CONTENT_BYTES, Task, TaskArtifact, TaskComment,
    TaskHistoryEntry, TaskStatus, artifact_presentation, resolve_task_dependencies,
    resolve_task_relations, serialize_task_artifacts, task_show_record_field_json,
    unknown_task_show_field_message,
};
use serde_json::{Map, Value, json};

use crate::OrbitRuntime;
use crate::TaskCrewRead;
use crate::application::task::TaskLintReport;

pub(super) fn task_to_json(task: &Task, status_by_id: &BTreeMap<String, TaskStatus>) -> Value {
    json!({
        "id": task.id,
        "parent_id": task.parent_id(),
        "title": task.title,
        "description": task.description,
        "acceptance_criteria": task.acceptance_criteria,
        "dependencies": task.dependencies(),
        "resolved_dependencies": resolve_task_dependencies(task, status_by_id)
            .into_iter()
            .map(|dependency| dependency.label())
            .collect::<Vec<_>>(),
        "tags": task.tags,
        "required_tools": task.required_tools,
        "plan": task.plan,
        "execution_summary": task.execution_summary,
        "context_files": task.context_files,
        "created_by": task.created_by,
        "planned_by": task.planned_by,
        "implemented_by": task.implemented_by,
        "status": task.status.to_string(),
        "priority": task.priority.to_string(),
        "complexity": task.complexity.map(|value| value.to_string()),
        "type": task.task_type.to_string(),
        "pr_status": task.pr_status,
        "external_refs": task.external_refs,
        "relations": resolve_task_relations(task, status_by_id),
        "source_task_id": task.source_task_id(),
        "job_run_id": task.job_run_id,
        "crew": task.crew,
        "orchestrator": task.orchestrator,
        "created_at": task.created_at.to_rfc3339(),
        "updated_at": task.updated_at.to_rfc3339(),
    })
}

pub(super) fn serialize_task(runtime: &OrbitRuntime, task: &Task) -> Result<Value, OrbitError> {
    let status_by_id = runtime.task_status_index()?;
    let mut value = task_to_json(task, &status_by_id);
    let object = value.as_object_mut().ok_or_else(|| {
        OrbitError::Execution("task JSON projection did not produce an object".to_string())
    })?;
    object.insert(
        "comments".to_string(),
        serialize_comments(&runtime.get_task_comments(&task.id)?)?,
    );
    object.insert(
        "history".to_string(),
        serialize_history(&runtime.get_task_history(&task.id)?)?,
    );
    insert_resolved_crew(runtime, task, object);
    Ok(value)
}

/// Enrich a task projection with its resolved crew, when this host can resolve
/// one.
///
/// `crew` (the stored value) is part of the record; `resolved_crew` /
/// `crew_model` only annotate it. A host whose `[crews.*]` table has no entry
/// for the stored crew — a task authored elsewhere, or a config edited since —
/// still owes the caller the task, so an unresolvable crew is reported as
/// `crew_unresolved` rather than failing the whole readout (ORB-10968). The
/// tolerance lives in `OrbitRuntime::task_crew_read`, shared with the CLI.
fn insert_resolved_crew(runtime: &OrbitRuntime, task: &Task, object: &mut Map<String, Value>) {
    match runtime.task_crew_read(task) {
        TaskCrewRead::Absent => {}
        TaskCrewRead::Resolved(projection) => {
            object.insert("resolved_crew".to_string(), Value::String(projection.name));
            object.insert("crew_model".to_string(), Value::String(projection.model));
        }
        TaskCrewRead::Unresolved { reason } => {
            object.insert("crew_unresolved".to_string(), Value::String(reason));
        }
    }
}

pub(super) fn serialize_task_lint_report(report: &TaskLintReport) -> Result<Value, OrbitError> {
    serde_json::to_value(report).map_err(serialize_error("serialize task lint report"))
}

pub(super) fn task_fields_to_json(
    runtime: &OrbitRuntime,
    task: &Task,
    fields: &[String],
) -> Result<Value, OrbitError> {
    let status_by_id = if fields
        .iter()
        .any(|field| matches!(field.as_str(), "resolved_dependencies" | "relations"))
    {
        Some(runtime.task_status_index()?)
    } else {
        None
    };

    if fields.len() == 1 {
        return task_field_to_json(runtime, task, &fields[0], status_by_id.as_ref());
    }

    let mut object = Map::new();
    for field in fields {
        object.insert(
            field.clone(),
            task_field_to_json(runtime, task, field, status_by_id.as_ref())?,
        );
    }
    Ok(Value::Object(object))
}

fn task_field_to_json(
    runtime: &OrbitRuntime,
    task: &Task,
    field: &str,
    status_by_id: Option<&BTreeMap<String, TaskStatus>>,
) -> Result<Value, OrbitError> {
    match field {
        "comments" => serialize_comments(&runtime.get_task_comments(&task.id)?),
        "plan" => Ok(Value::String(task.plan.clone())),
        "execution_summary" => Ok(Value::String(task.execution_summary.clone())),
        "description" => Ok(Value::String(task.description.clone())),
        "acceptance_criteria" => serde_json::to_value(&task.acceptance_criteria)
            .map_err(serialize_error("serialize acceptance criteria")),
        "dependencies" => serde_json::to_value(task.dependencies())
            .map_err(serialize_error("serialize dependencies")),
        "tags" => serde_json::to_value(&task.tags).map_err(serialize_error("serialize tags")),
        "required_tools" => serde_json::to_value(&task.required_tools)
            .map_err(serialize_error("serialize required tools")),
        "resolved_dependencies" => serde_json::to_value(
            resolve_task_dependencies(
                task,
                status_by_id.ok_or_else(|| {
                    OrbitError::Execution(
                        "missing dependency status index for resolved_dependencies".to_string(),
                    )
                })?,
            )
            .into_iter()
            .map(|dependency| dependency.label())
            .collect::<Vec<_>>(),
        )
        .map_err(serialize_error("serialize resolved dependencies")),
        "relations" => serde_json::to_value(resolve_task_relations(
            task,
            status_by_id.ok_or_else(|| {
                OrbitError::Execution("missing task status index for relations".to_string())
            })?,
        ))
        .map_err(serialize_error("serialize relations")),
        "history" => serialize_history(&runtime.get_task_history(&task.id)?),
        "context_files" => serde_json::to_value(&task.context_files)
            .map_err(serialize_error("serialize context files")),
        "crew" => serde_json::to_value(&task.crew).map_err(serialize_error("serialize crew")),
        "orchestrator" => serde_json::to_value(&task.orchestrator)
            .map_err(serialize_error("serialize orchestrator")),
        "artifacts" => Ok(serialize_task_artifacts(
            &runtime.get_task_artifact_manifest(&task.id)?,
        )),
        other => task_show_record_field_json(task, other)
            .ok_or_else(|| OrbitError::InvalidInput(unknown_task_show_field_message(other))),
    }
}

fn serialize_comments(comments: &[TaskComment]) -> Result<Value, OrbitError> {
    serde_json::to_value(comments).map_err(serialize_error("serialize comments"))
}

fn serialize_history(history: &[TaskHistoryEntry]) -> Result<Value, OrbitError> {
    serde_json::to_value(history).map_err(serialize_error("serialize history"))
}

pub(super) fn serialize_error(label: &'static str) -> impl FnOnce(serde_json::Error) -> OrbitError {
    move |error| OrbitError::Execution(format!("{label}: {error}"))
}

/// Project one stored artifact into the read payload returned by
/// `orbit.task.artifact.get`.
///
/// The bytes are carried in exactly one field so a caller never has to guess
/// which to trust: `content` for UTF-8 text, `content_base64` otherwise.
/// `presentation` states whether the payload may be rendered, and is the same
/// classification the dashboard applies, so the two surfaces cannot disagree
/// about whether something is a safe image.
///
/// Bounded by the shared attach limit. An oversize artifact is a clear error
/// rather than a truncated payload, because a partial image is indistinguishable
/// from a corrupt one; the dashboard download route still serves it whole.
pub(super) fn serialize_task_artifact_read(
    task_id: &str,
    artifact: &TaskArtifact,
) -> Result<Value, OrbitError> {
    let size = artifact.content.len();
    if size as u64 > MAX_TASK_ARTIFACT_CONTENT_BYTES {
        return Err(OrbitError::InvalidInput(format!(
            "artifact '{}' on task '{task_id}' is {size} bytes, over the \
             {MAX_TASK_ARTIFACT_CONTENT_BYTES} byte inline read limit; download it from \
             /api/tasks/{task_id}/artifacts/{} instead",
            artifact.path, artifact.path
        )));
    }

    let presentation = artifact_presentation(&artifact.media_type, &artifact.content);
    let mut object = Map::new();
    object.insert("id".to_string(), Value::String(task_id.to_string()));
    object.insert("path".to_string(), Value::String(artifact.path.clone()));
    object.insert(
        "media_type".to_string(),
        Value::String(artifact.media_type.clone()),
    );
    object.insert("size".to_string(), Value::Number(size.into()));
    if let Some(created_by) = &artifact.created_by {
        object.insert("created_by".to_string(), Value::String(created_by.clone()));
    }
    object.insert(
        "presentation".to_string(),
        Value::String(presentation.as_str().to_string()),
    );
    match presentation {
        ArtifactPresentation::Text => {
            let content = artifact.text_content().ok_or_else(|| {
                OrbitError::Execution(format!(
                    "artifact '{}' on task '{task_id}' classified as text but is not UTF-8",
                    artifact.path
                ))
            })?;
            object.insert("encoding".to_string(), Value::String("utf8".to_string()));
            object.insert("content".to_string(), Value::String(content.to_string()));
        }
        ArtifactPresentation::Image | ArtifactPresentation::Opaque => {
            object.insert("encoding".to_string(), Value::String("base64".to_string()));
            object.insert(
                "content_base64".to_string(),
                Value::String(BASE64_STANDARD.encode(&artifact.content)),
            );
        }
    }
    Ok(Value::Object(object))
}
