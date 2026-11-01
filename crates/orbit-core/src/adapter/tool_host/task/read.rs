//! Handler for the task artifact read verb.

use orbit_common::OrbitError;
use orbit_common::governance::operation::OperationInput;
use orbit_common::governance::task::artifact_listing_hint;
use orbit_types::task::validate_relative_artifact_path;
use orbit_types::tool::ToolSessionContext;
use serde_json::Value;

use crate::OrbitRuntime;

use super::super::json::serialize_task_artifact_read;

/// Read one stored artifact's bytes through the task's own artifact owner.
///
/// Discovery stays separate from retrieval: the task's artifact listing shows
/// compact metadata, and only this call pays for a payload. Path containment,
/// workspace ownership, and the symlink-safe blob resolve all belong to the
/// store, so this handler adds no second access rule of its own — a caller can
/// only ever reach an artifact through the task that owns it.
pub(super) fn artifact_get(
    runtime: &OrbitRuntime,
    session: &ToolSessionContext,
    input: &OperationInput,
) -> Result<Value, OrbitError> {
    let id = input.required_string("id")?;
    let listing_hint = artifact_listing_hint(session);
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
    let artifact = runtime
        .get_task_artifact_inline(&task.id, &path)?
        .ok_or_else(|| {
            OrbitError::not_found(
                orbit_common::NotFoundKind::Artifact,
                format!("{}/{path}; {listing_hint}", task.id),
            )
        })?;
    serialize_task_artifact_read(&task.id, &artifact)
}

impl OrbitRuntime {
    /// Run a read callback that application consent admitted: `orbit.task.show`
    /// limited to the admitted fields (the application ceiling when the call
    /// names none), and `orbit.task.artifact.get`, whose returned bytes are
    /// rechecked against the digest the entry bound so a write between
    /// admission and read is refused. `None` for any other tool or an
    /// ordinary caller.
    pub(crate) fn execute_application_read(
        &self,
        tool: &str,
        input: &Value,
        session: &ToolSessionContext,
    ) -> Result<Option<Value>, OrbitError> {
        use orbit_common::governance::authorization::APPLICATION_TASK_SHOW_FIELDS;

        let Some(invocation) = &session.application else {
            return Ok(None);
        };
        if !matches!(tool, "orbit.task.show" | "orbit.task.artifact.get") {
            return Ok(None);
        }
        self.check_tool_enabled(tool)?;
        let string = |key: &str| {
            input
                .get(key)
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    OrbitError::InvalidInput(format!("application read needs a `{key}` string"))
                })
        };
        let task = self.get_task(string("id")?)?;
        let output = if tool == "orbit.task.show" {
            let fields: Vec<String> = match input.get("fields") {
                None => APPLICATION_TASK_SHOW_FIELDS
                    .iter()
                    .map(ToString::to_string)
                    .collect(),
                Some(Value::Array(items)) => items
                    .iter()
                    .map(|item| item.as_str().map(str::to_owned))
                    .collect::<Option<_>>()
                    .ok_or_else(|| {
                        OrbitError::InvalidInput("application read `fields` are strings".into())
                    })?,
                Some(_) => {
                    return Err(OrbitError::InvalidInput(
                        "application read `fields` is an array".into(),
                    ));
                }
            };
            if fields.is_empty()
                || fields
                    .iter()
                    .any(|field| !APPLICATION_TASK_SHOW_FIELDS.contains(&field.as_str()))
            {
                return Err(OrbitError::CapabilityDenied(
                    "application task reads are limited to the application task.show fields".into(),
                ));
            }
            let projected = super::super::json::task_fields_to_json(self, &task, &fields)?;
            if fields.as_slice() == ["terminal"] {
                serde_json::json!({ "terminal": projected })
            } else {
                projected
            }
        } else {
            let path = string("path")?;
            validate_relative_artifact_path(path)
                .map_err(|error| OrbitError::InvalidInput(error.to_string()))?;
            let artifact = self
                .get_task_artifact_inline(&task.id, path)?
                .ok_or_else(|| {
                    OrbitError::not_found(
                        orbit_common::NotFoundKind::Artifact,
                        format!("{}/{path}", task.id),
                    )
                })?;
            if let Some(evidence) = &invocation.predicate
                && let Some(bound) = &evidence.artifact
                && evidence.task == task.id
                && bound.path == path
                && orbit_common::security::release::sha256_hex(&artifact.content) != bound.sha256
            {
                return Err(OrbitError::CapabilityDenied(
                    "bound artifact changed since the application entry was admitted".into(),
                ));
            }
            serialize_task_artifact_read(&task.id, &artifact)?
        };
        Ok(Some(
            orbit_common::security::redaction::redact_sensitive_env_json(output),
        ))
    }
}
