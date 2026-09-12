use orbit_common::{NotFoundKind, OrbitError};
use rmcp::model::CallToolResult;
use serde_json::{Value, json};

/// Map an [`OrbitError`] from tool execution into an `isError: true` MCP
/// [`CallToolResult`] with a structured payload.
///
/// The payload always carries:
/// - `code`: a short, stable machine-readable classifier (e.g. `"not_found"`,
///   `"invalid_input"`). Callers match on this rather than the free-form text.
/// - `message`: the human-readable error message (the `Display` of the error).
pub(crate) fn tool_error_result(err: &OrbitError) -> CallToolResult {
    CallToolResult::structured_error(error_payload(err))
}

fn error_payload(err: &OrbitError) -> Value {
    if let OrbitError::RemoteTool { payload, .. } = err {
        return payload.clone();
    }
    let mut payload = json!({
        "code": error_code(err),
        "message": err.to_string(),
    });
    if let Some(did_you_mean) = err.did_you_mean()
        && let Some(object) = payload.as_object_mut()
    {
        object.insert("did_you_mean".to_string(), json!(did_you_mean));
    }
    if let Some(artifact_origin) = err.artifact_origin()
        && let Some(object) = payload.as_object_mut()
    {
        object.insert("artifact_origin".to_string(), json!(artifact_origin));
    }
    // [ORB-10544] A ship duplicate-dispatch refusal names the contended task and
    // the run holding it, so a tool caller can wait on or cancel that run
    // without parsing the message — the same pair the dashboard's 409 carries.
    if let Some((task_id, run_id)) = err.ship_run_in_flight()
        && let Some(object) = payload.as_object_mut()
    {
        object.insert("task_id".to_string(), json!(task_id));
        object.insert("run_id".to_string(), json!(run_id));
    }
    if let Some((task_id, path, reason)) = err.task_bundle_corruption()
        && let Some(object) = payload.as_object_mut()
    {
        object.insert("task_id".to_string(), json!(task_id));
        object.insert("path".to_string(), json!(path));
        object.insert("reason".to_string(), json!(reason));
    }
    if let Some(details) = err.friction_not_local_details()
        && let Some(object) = payload.as_object_mut()
    {
        object.insert("friction_id".to_string(), json!(details.friction_id));
        object.insert("task_id".to_string(), json!(details.task_id));
        object.insert("workspace_id".to_string(), json!(details.workspace_id));
        object.insert("found_in".to_string(), json!(details.found_in));
    }
    payload
}

fn error_code(err: &OrbitError) -> &str {
    match err {
        OrbitError::NotFound { kind, .. } => match kind {
            NotFoundKind::Tool => "tool_not_found",
            NotFoundKind::Task
            | NotFoundKind::Artifact
            | NotFoundKind::Skill
            | NotFoundKind::Job
            | NotFoundKind::JobRun
            | NotFoundKind::Activity
            | NotFoundKind::Adr
            | NotFoundKind::DesignFeature
            | NotFoundKind::AgentSession
            | NotFoundKind::Workspace => "not_found",
        },
        OrbitError::CompanionNotInstalled(_) => "companion_not_installed",
        OrbitError::PolicyDenied(_) => "policy_denied",
        OrbitError::CapabilityDenied(_) => "capability_denied",
        OrbitError::UnknownSelector(_) => "unknown_selector",
        OrbitError::AmbiguousDestination(_) => "ambiguous_destination",
        OrbitError::AmbiguousCaller(_) => "ambiguous_caller",
        OrbitError::UnreachableDestination(_) => "unreachable_destination",
        OrbitError::StaleRoute(_) => "stale_route",
        OrbitError::UnhealthyCheckout(_) => "unhealthy_checkout",
        OrbitError::ToolNotOnThisHost(_) => "tool_not_on_this_host",
        OrbitError::CapabilityRefused(_) => "capability_refused",
        OrbitError::InvalidInput(_) | OrbitError::InvalidInputDiagnostic { .. } => "invalid_input",
        OrbitError::SensitiveInput { .. } => "sensitive_input",
        OrbitError::SkillValidation(_) | OrbitError::JobValidation(_) => "validation_failed",
        OrbitError::TaskStatusTransition(_)
        | OrbitError::JobRunStateTransition(_)
        | OrbitError::AdrInvalidTransition(_) => "invalid_transition",
        // [ORB-10965] Not "invalid_transition": a duplicate start losing to the
        // incumbent owner is an expected race, and the caller yields rather
        // than treating its own request as malformed.
        OrbitError::JobRunStartConflict(_) => "job_run_start_conflict",
        // [ORB-11253, ORB-12260] A compare-and-set revision conflict on a
        // run-control update or operation grant transition.
        OrbitError::JobRunControlConflict(_) => "conflict",
        OrbitError::DependencyNotDelivered { .. } => "dependency_not_delivered",
        OrbitError::ShipRunInFlight { .. } => "ship_run_in_flight",
        OrbitError::WorkspaceClaimHeld(_) => "workspace_claim_held",
        OrbitError::RemoteArtifactUnavailable { .. } => "remote_artifact_unavailable",
        OrbitError::ArtifactNotLocal { .. } => "artifact_not_local",
        OrbitError::FrictionNotLocal(_) => "friction_not_local",
        OrbitError::AgentProtocolViolation(_) => "agent_protocol_violation",
        OrbitError::UnsupportedAgentProvider(_) => "unsupported_provider",
        OrbitError::OwnerUnavailable(_) => "owner_unavailable",
        OrbitError::OwnerNegotiation(_) => "owner_negotiation",
        OrbitError::OutcomeUnknown { .. } => "outcome_unknown",
        OrbitError::RemoteTool { code, .. } => code.as_str(),
        OrbitError::Execution(_) => "execution_failed",
        OrbitError::TaskBundleCorrupt { .. } => "task_bundle_corrupt",
        OrbitError::Store(_) => "store_error",
        OrbitError::WorkspaceError(_) => "workspace_error",
        OrbitError::Io(_) => "io_error",
        OrbitError::Migration(_) => "migration_failed",
        // OrbitError is non-exhaustive so newly added errors can cross this
        // crate boundary without forcing an MCP release. Unknown variants are
        // intentionally classified conservatively until given a stable code.
        _ => "internal_error",
    }
}

#[cfg(test)]
#[path = "tests/error.rs"]
mod tests;
