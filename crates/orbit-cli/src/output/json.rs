use orbit_core::{NotFoundKind, OrbitError};
use serde_json::{Value, json};

pub fn print_with_format(value: &Value, pretty: bool) -> Result<(), OrbitError> {
    println!("{}", render(value, pretty)?);
    Ok(())
}

pub fn render(value: &Value, pretty: bool) -> Result<String, OrbitError> {
    if pretty {
        serde_json::to_string_pretty(value).map_err(|e| OrbitError::Execution(e.to_string()))
    } else {
        serde_json::to_string(value).map_err(|e| OrbitError::Execution(e.to_string()))
    }
}

pub fn error_payload(error: &OrbitError) -> Value {
    if let OrbitError::RemoteTool { payload, .. } = error {
        return payload.clone();
    }
    let mut payload = json!({
        "error": error.to_string(),
        "code": error_code(error),
    });
    if let Some(did_you_mean) = error.did_you_mean()
        && let Some(object) = payload.as_object_mut()
    {
        object.insert("did_you_mean".to_string(), json!(did_you_mean));
    }
    if let Some(artifact_origin) = error.artifact_origin()
        && let Some(object) = payload.as_object_mut()
    {
        object.insert("artifact_origin".to_string(), json!(artifact_origin));
    }
    if let Some((task_id, path, reason)) = error.task_bundle_corruption()
        && let Some(object) = payload.as_object_mut()
    {
        object.insert("task_id".to_string(), json!(task_id));
        object.insert("path".to_string(), json!(path));
        object.insert("reason".to_string(), json!(reason));
    }
    if let Some(details) = error.friction_not_local_details()
        && let Some(object) = payload.as_object_mut()
    {
        object.insert("friction_id".to_string(), json!(details.friction_id));
        object.insert("task_id".to_string(), json!(details.task_id));
        object.insert("workspace_id".to_string(), json!(details.workspace_id));
        object.insert("found_in".to_string(), json!(details.found_in));
    }
    payload
}

fn error_code(error: &OrbitError) -> &str {
    match error {
        OrbitError::PolicyDenied(_) => "policy_denied",
        OrbitError::NotFound { kind, .. } => match kind {
            NotFoundKind::Tool => "tool_not_found",
            NotFoundKind::Task => "task_not_found",
            NotFoundKind::Artifact => "task_artifact_not_found",
            NotFoundKind::Skill => "skill_not_found",
            NotFoundKind::Job => "job_not_found",
            NotFoundKind::JobRun => "job_run_not_found",
            NotFoundKind::Activity => "activity_not_found",
            // ADR reads expose the same stable distinction as the tool and
            // federation contracts: absent allocations are `not_found`, while
            // allocated-but-unreadable bodies are `remote_artifact_unavailable`.
            NotFoundKind::Adr => "not_found",
            NotFoundKind::DesignFeature => "design_feature_not_found",
            NotFoundKind::AgentSession => "agent_session_not_found",
            NotFoundKind::Workspace => "workspace_not_found",
        },
        OrbitError::CapabilityDenied(_) => "capability_denied",
        // Catalog-role refusal, not an operator capability grant: the CLI
        // surfaces the same stable code MCP does [ORB-11012].
        OrbitError::CapabilityRefused(_) => "capability_refused",
        OrbitError::CompanionNotInstalled(_) => "companion_not_installed",
        OrbitError::InvalidInput(_) | OrbitError::InvalidInputDiagnostic { .. } => "invalid_input",
        OrbitError::SensitiveInput { .. } => "sensitive_input",
        OrbitError::SkillValidation(_) => "skill_validation_failed",
        OrbitError::JobValidation(_) => "job_validation_failed",
        OrbitError::AgentProtocolViolation(_) => "agent_protocol_violation",
        OrbitError::UnsupportedAgentProvider(_) => "unsupported_agent_provider",
        OrbitError::OwnerUnavailable(_) => "owner_unavailable",
        OrbitError::OwnerNegotiation(_) => "owner_negotiation",
        OrbitError::OutcomeUnknown { .. } => "outcome_unknown",
        OrbitError::RemoteTool { code, .. } => code.as_str(),
        OrbitError::Execution(_) => "execution_failed",
        OrbitError::TaskBundleCorrupt { .. } => "task_bundle_corrupt",
        OrbitError::Store(_) => "store_error",
        OrbitError::TaskStatusTransition(_) => "task_status_transition",
        OrbitError::JobRunStateTransition(_) => "job_run_state_transition",
        OrbitError::JobRunStartConflict(_) => "job_run_start_conflict",
        OrbitError::JobRunControlConflict(_) => "conflict",
        OrbitError::WorkspaceError(_) => "workspace_error",
        OrbitError::Io(_) => "io_error",
        OrbitError::AdrInvalidTransition(_) => "adr_invalid_transition",
        OrbitError::RemoteArtifactUnavailable { .. } => "remote_artifact_unavailable",
        OrbitError::ArtifactNotLocal { .. } => "artifact_not_local",
        OrbitError::FrictionNotLocal(_) => "friction_not_local",
        OrbitError::Migration(_) => "migration_failed",
        // New OrbitError variants must remain JSON-serializable before this
        // boundary assigns them a dedicated stable code.
        _ => "internal_error",
    }
}
