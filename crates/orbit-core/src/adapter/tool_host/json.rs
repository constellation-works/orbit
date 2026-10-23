use std::collections::{BTreeMap, BTreeSet};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use orbit_common::OrbitError;
use orbit_types::task::{
    ArtifactPresentation, Task, TaskArtifact, TaskStatus, artifact_presentation,
    is_valid_orb_task_id, task_base_json,
};
use serde_json::{Map, Value};

use crate::OrbitRuntime;
use crate::application::task::{
    TaskLintReport, TaskOsReadout, task_record_json, task_show_field_json, task_show_json,
};

pub(super) fn serialize_task(runtime: &OrbitRuntime, task: &Task) -> Result<Value, OrbitError> {
    serialize_task_record(runtime, task, true)
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

/// Serialize a task returned by a mutating tool.
///
/// Write responses carry the base task record and its crew enrichment by
/// default. The sidecars (`comments`, `history`, `artifacts`) are included only
/// when the caller explicitly projects them with `fields` or `field`, keeping a
/// write from hydrating records it does not need; the `review` gate stays on
/// the full `orbit.task.show` record, available through [`serialize_task`].
pub(super) fn serialize_task_write_response(
    runtime: &OrbitRuntime,
    task: &Task,
    fields: Option<&[String]>,
) -> Result<Value, OrbitError> {
    match fields {
        Some(fields) => task_fields_to_json(runtime, task, fields),
        None => serialize_task_record(runtime, task, false),
    }
}

fn serialize_task_record(
    runtime: &OrbitRuntime,
    task: &Task,
    include_sidecars: bool,
) -> Result<Value, OrbitError> {
    let status_by_id = task_reference_status_index(runtime, task)?;
    if include_sidecars {
        let row = runtime.task_row_with_sidecars(task.clone())?;
        return task_record_json(runtime, &row, &status_by_id).map(Value::Object);
    }
    let mut object = task_base_json(task, &status_by_id);
    TaskOsReadout::new(runtime)?.insert_into(task, &mut object);
    runtime.task_crew_read(task).insert_into(&mut object);
    Ok(Value::Object(object))
}

/// Resolve only the referenced task IDs needed by one task projection.
///
/// The list surface already owns a bounded status page and passes its index to
/// [`task_base_json`]. Individual task reads use the same registry projection,
/// bounded to the handful of dependency and relation targets, so they resolve
/// cross-workspace references without hydrating their bundles.
///
/// Relation targets are not always task ids — a `resolves` relation may
/// legitimately point at a friction, ADR, or learning id (e.g.
/// `F2026-05-001`). Only ids that parse as task ids are point-read; other
/// namespaces are skipped rather than sent through `get_task_row`, which
/// rejects them as invalid input instead of reporting them not found.
fn task_reference_status_index(
    runtime: &OrbitRuntime,
    task: &Task,
) -> Result<BTreeMap<String, TaskStatus>, OrbitError> {
    let referenced_ids = task
        .dependencies()
        .into_iter()
        .chain(
            task.relations
                .iter()
                .map(|relation| relation.target.clone()),
        )
        .filter(|id| is_valid_orb_task_id(id))
        .collect::<BTreeSet<_>>();
    let mut status_by_id = runtime.task_status_index_for(&referenced_ids)?;
    runtime.satisfy_completed_archived_dependencies(&mut status_by_id, [task]);
    Ok(status_by_id)
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
        Some(task_reference_status_index(runtime, task)?)
    } else {
        None
    };

    if fields.len() == 1 {
        return task_show_field_json(runtime, task, &fields[0], status_by_id.as_ref());
    }

    let mut object = Map::new();
    for field in fields {
        object.insert(
            field.clone(),
            task_show_field_json(runtime, task, field, status_by_id.as_ref())?,
        );
    }
    Ok(Value::Object(object))
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
