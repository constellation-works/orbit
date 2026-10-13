use orbit_common::OrbitError;
use orbit_common::governance::operation::OperationInput;
use orbit_common::protocol::tool_input::optional_csv_or_string_list_alias;
use orbit_types::task::{
    is_task_show_projection_field, unknown_task_show_field_message, validate_relative_artifact_path,
};
use serde_json::Value;

use crate::OrbitRuntime;

use super::json::serialize_task_artifact_read;

/// The persisted identity travels beside the caller's response projection so
/// redaction audit attribution does not depend on which fields were requested.
pub(super) struct TaskWriteOutput {
    pub response: Value,
    pub persisted_id: String,
}

/// Read one stored artifact's bytes through the task's own artifact owner.
///
/// Discovery stays separate from retrieval: `orbit.task.show` with
/// `field: "artifacts"` lists compact metadata, and only this call pays for a
/// payload. Path containment, workspace ownership, and the symlink-safe blob
/// resolve all belong to the store, so this handler adds no second access
/// rule of its own — a caller can only ever reach an artifact through the task
/// that owns it.
pub(super) fn artifact_get(
    runtime: &OrbitRuntime,
    input: &OperationInput,
) -> Result<Value, OrbitError> {
    let id = input.required_string("id")?;
    let listing_hint = "list available artifacts with orbit.task.show and field: \"artifacts\"";
    let path = input
        .required_string("path")
        .map_err(|error| OrbitError::InvalidInput(format!("{error}; {listing_hint}")))?;
    if path == "." {
        return Err(OrbitError::InvalidInput(format!(
            "`path` must name one artifact, not a directory; {listing_hint}"
        )));
    }
    validate_relative_artifact_path(&path)
        .map_err(|error| OrbitError::InvalidInput(error.to_string()))?;
    // Resolve the task first so an unknown or foreign id fails as not-found
    // before any artifact lookup reports on a task the caller cannot see.
    let task = runtime.get_task(&id)?;
    let artifact = runtime.get_task_artifact(&task.id, &path)?.ok_or_else(|| {
        OrbitError::not_found(
            orbit_common::NotFoundKind::Artifact,
            format!("{}/{path}", task.id),
        )
    })?;
    serialize_task_artifact_read(&task.id, &artifact)
}

/// Read-only projection controls are accepted by mutating task tools solely
/// for shaping their response. In particular, `comments` and `history` are
/// omitted from the default write response and can be requested explicitly.
pub(super) fn write_response_fields(input: &Value) -> Result<Option<Vec<String>>, OrbitError> {
    let fields = optional_csv_or_string_list_alias(input, &["fields", "field"])?;
    if let Some(fields) = &fields
        && let Some(unknown) = fields
            .iter()
            .find(|field| !is_task_show_projection_field(field))
    {
        return Err(OrbitError::InvalidInput(unknown_task_show_field_message(
            unknown,
        )));
    }
    Ok(fields)
}
