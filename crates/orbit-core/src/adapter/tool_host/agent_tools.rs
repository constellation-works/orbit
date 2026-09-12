//! `orbit.agent.invoke` protocol translation [ORB-11354].
//!
//! Parses the tool payload and hands it to
//! [`OrbitRuntime::submit_agent_invoke_run`], which owns the admission and
//! every validation rule. Nothing here decides anything.

use orbit_common::OrbitError;
use orbit_common::protocol::tool_input::{optional_string, required_string};
use orbit_types::tool::ToolSessionContext;
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::job::AgentInvokeRequest;

pub(super) fn invoke(
    runtime: &OrbitRuntime,
    session_context: &ToolSessionContext,
    input: Value,
    agent: Option<String>,
    model: Option<String>,
) -> Result<Value, OrbitError> {
    let prompt = required_string(&input, &["prompt"], "prompt")?;
    let cwd = required_string(&input, &["cwd"], "cwd")?;
    let crew = optional_string(&input, "crew")?;
    let idempotency_key = optional_string(&input, "idempotency_key")?;
    let provider_sandbox = optional_string(&input, "provider_sandbox")?;
    let timeout_seconds = parse_timeout(&input)?;
    let actor = orbit_types::identity::normalize_optional_attribution_label(
        model.as_deref().or(agent.as_deref()),
        model.as_deref(),
    );

    let submission = runtime.submit_agent_invoke_run(AgentInvokeRequest {
        prompt: &prompt,
        cwd: &cwd,
        crew: crew.as_deref(),
        timeout_seconds,
        idempotency_key: idempotency_key.as_deref(),
        provider_sandbox: provider_sandbox.as_deref(),
        actor: actor.as_deref(),
        session_context,
    })?;

    Ok(json!({
        "run_id": submission.run_id,
        "job_id": submission.job_id,
        "submitted_at": submission.submitted_at,
        "state": if submission.queued { "queued" } else { "submitted" },
        "deduplicated": submission.deduplicated,
        "timeout_seconds": submission.timeout_seconds,
        "authorized_by": submission.admission.authorized_by,
        "authorizer_provenance": submission.admission.authorizer_provenance,
        "caller_machine_id": submission.admission.caller_machine_id,
        "caller_identity": submission.admission.caller_identity,
        "agent_invoke_mode": submission.admission.agent_invoke_mode,
        "workspace_path": submission.admission.workspace_path,
        "cwd": submission.admission.cwd,
        "sandboxed": false,
        "provider_sandbox": submission.provider_sandbox,
        "warnings": submission.warnings,
    }))
}

fn parse_timeout(input: &Value) -> Result<Option<u64>, OrbitError> {
    match input.get("timeout_seconds") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value.as_u64().map(Some).ok_or_else(|| {
            OrbitError::InvalidInput("`timeout_seconds` must be a non-negative integer".to_string())
        }),
    }
}
