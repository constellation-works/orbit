use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use orbit_common::OrbitError;
use orbit_types::task::{ArtifactPresentation, Task, TaskArtifact, artifact_presentation};
use serde_json::{Map, Value};

use crate::OrbitRuntime;
use crate::application::task::{
    TaskLintReport, task_fields_response_json, task_record_json, task_reference_status_index,
    task_show_json,
};

pub(super) fn serialize_task(runtime: &OrbitRuntime, task: &Task) -> Result<Value, OrbitError> {
    let status_by_id = task_reference_status_index(runtime, task)?;
    let row = runtime.task_row_with_sidecars(task.clone())?;
    task_record_json(runtime, &row, &status_by_id).map(Value::Object)
}

/// The full `orbit.task.show` record. This is [`task_show_json`] and nothing
/// else: `next_step` and `final_recovery` are part of that record, present
/// only when they apply.
pub(super) fn serialize_task_show(
    runtime: &OrbitRuntime,
    task: &Task,
) -> Result<Value, OrbitError> {
    let status_by_id = task_reference_status_index(runtime, task)?;
    let row = runtime.task_row_with_sidecars(task.clone())?;
    task_show_json(runtime, &row, &status_by_id).map(Value::Object)
}

pub(super) fn serialize_task_lint_report(report: &TaskLintReport) -> Result<Value, OrbitError> {
    serde_json::to_value(report).map_err(serialize_error("serialize task lint report"))
}

pub(super) fn task_fields_to_json(
    runtime: &OrbitRuntime,
    task: &Task,
    fields: &[String],
) -> Result<Value, OrbitError> {
    task_fields_response_json(runtime, task, fields)
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
    artifact.validate_inline_size(task_id)?;

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
