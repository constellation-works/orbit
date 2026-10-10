//! Desktop transport translation; application/store own mutation invariants.
use crate::OrbitRuntime;
use orbit_common::OrbitError;
use orbit_common::protocol::tool_input::required_string;
use orbit_types::desktop::DesktopTaskRequest;
use orbit_types::tool::ToolSessionContext;
use serde_json::Value;

pub(in crate::adapter::tool_host) fn read(
    runtime: &OrbitRuntime,
    session: &ToolSessionContext,
    input: Value,
) -> Result<Value, OrbitError> {
    super::read::read(runtime, session, input)
}
pub(in crate::adapter::tool_host) fn snapshot(
    runtime: &OrbitRuntime,
    session: &ToolSessionContext,
    input: Value,
) -> Result<Value, OrbitError> {
    let id = required_string(&input, &["id"], "id")?;
    let mut value = serde_json::to_value(runtime.desktop_task_snapshot(&id, session)?)
        .map_err(|error| OrbitError::Execution(format!("serialize desktop response: {error}")))?;
    value["workspace"] = input["workspace"].clone();
    Ok(value)
}
pub(in crate::adapter::tool_host) fn write(
    runtime: &OrbitRuntime,
    session: &ToolSessionContext,
    mut input: Value,
    agent: Option<String>,
    model: Option<String>,
) -> Result<Value, OrbitError> {
    let workspace = input["workspace"].clone();
    if let Some(object) = input.as_object_mut() {
        object.remove("workspace");
        object.remove("model");
    }
    let request: DesktopTaskRequest = serde_json::from_value(input)
        .map_err(|error| OrbitError::InvalidInput(format!("invalid desktop operation: {error}")))?;
    let outcome = runtime.desktop_task_write(request, agent, model, session);
    let mut value = match outcome {
        Ok(result) => serde_json::to_value(result),
        Err(OrbitError::DesktopWriteAccepted { task_id, reason }) => {
            Ok(serde_json::json!({"accepted":true,"task_id":task_id,
                "refresh_error":orbit_common::security::redaction::redact_all(&reason)}))
        }
        Err(OrbitError::TaskRevisionConflict { task_id }) => {
            let snapshot = runtime.desktop_task_snapshot(&task_id, session)?;
            Ok(serde_json::json!({
                "conflict": {"code":"revision_conflict", "message":"Task changed. Review the fresh snapshot before submitting again."},
                "snapshot": snapshot,
            }))
        }
        Err(error @ (OrbitError::InvalidInput(_) | OrbitError::InvalidInputDiagnostic { .. } | OrbitError::ClaimRefused { .. }
            | OrbitError::CapabilityDenied(_) | OrbitError::TaskStatusTransition(_))) => {
            // Core maps post-commit refresh failures to DesktopWriteAccepted. These
            // variants therefore prove validation refused before a mutation.
            Ok(serde_json::json!({"mutation_applied":false,
                "refusal":{"code":"desktop_validation_refused", "message":orbit_common::security::redaction::redact_all(&error.to_string())}}))
        }
        Err(error) => return Err(error),
    }.map_err(|error: serde_json::Error| OrbitError::Execution(format!("serialize desktop response: {error}")))?;
    value["workspace"] = workspace;
    Ok(value)
}
