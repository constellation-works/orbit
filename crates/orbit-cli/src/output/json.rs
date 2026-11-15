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
    let mut fields = error.wire_details();
    fields.insert("error".into(), json!(error.to_string()));
    fields.insert("code".into(), json!(error_code(error)));
    Value::Object(fields)
}

/// The error object for an argv clap rejected, in the same shape as
/// [`error_payload`]. There is no `OrbitError` behind it: parsing failed
/// before any command existed to return one.
pub fn usage_error_payload(message: &str) -> Value {
    json!({
        "error": message,
        "code": "usage_error",
    })
}

fn error_code(error: &OrbitError) -> &str {
    match error {
        OrbitError::NotFound { kind, .. } => match kind {
            NotFoundKind::Tool => "tool_not_found",
            NotFoundKind::Task => "task_not_found",
            NotFoundKind::Friction => "friction_not_found",
            NotFoundKind::Artifact => "task_artifact_not_found",
            NotFoundKind::Skill => "skill_not_found",
            NotFoundKind::Job => "job_not_found",
            NotFoundKind::JobRun => "job_run_not_found",
            NotFoundKind::Workspace => "workspace_not_found",
        },
        OrbitError::NoJobRuns => "job_run_not_found",
        OrbitError::SkillValidation(_) => "skill_validation_failed",
        OrbitError::JobValidation(_) | OrbitError::SourceRepairRunStateRefused { .. } => {
            "job_validation_failed"
        }
        OrbitError::SourceRepairAuthorityRefused(code) if !code.reports_as_execution() => {
            "job_validation_failed"
        }
        OrbitError::JobRunStateTransition(_) => "job_run_state_transition",
        OrbitError::UnsupportedAgentProvider(_) => "unsupported_agent_provider",
        _ => error.wire_code(),
    }
}
