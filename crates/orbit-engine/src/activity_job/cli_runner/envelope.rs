use orbit_agent::{AgentResponseStatus, parse_and_validate_response};
use orbit_types::telemetry::InvocationTrace;
use orbit_types::tool::ExecutionResult;
use orbit_types::workflow::activity_job::AgentLoopSpec;
use serde_json::Value;

use super::super::dispatcher::DispatchError;

pub(super) fn cli_agent_envelope_json(
    spec: &AgentLoopSpec,
    run_id: &str,
    input: &Value,
    task_ctx: Option<&Value>,
    requested_tools: &[String],
    effective_tools: &[String],
) -> Result<Vec<u8>, DispatchError> {
    let mut envelope = serde_json::Map::new();
    envelope.insert("schemaVersion".to_string(), Value::from(1));
    envelope.insert(
        "instruction".to_string(),
        Value::String(spec.instruction.clone()),
    );
    envelope.insert(
        "prompt".to_string(),
        Value::String(user_prompt_from_input(input)?),
    );
    envelope.insert("input".to_string(), input.clone());
    envelope.insert("run_id".to_string(), Value::String(run_id.to_string()));
    envelope.insert(
        "tools".to_string(),
        serde_json::to_value(effective_tools)
            .map_err(|err| DispatchError::CliInvocationFailed(format!("serialize tools: {err}")))?,
    );
    envelope.insert(
        "required_tools".to_string(),
        serde_json::to_value(requested_tools).map_err(|err| {
            DispatchError::CliInvocationFailed(format!("serialize required tools: {err}"))
        })?,
    );
    envelope.insert(
        "model".to_string(),
        serde_json::to_value(&spec.model)
            .map_err(|err| DispatchError::CliInvocationFailed(format!("serialize model: {err}")))?,
    );

    if let Some(task) = task_ctx {
        envelope.insert("task".to_string(), task.clone());
    }

    serde_json::to_vec(&Value::Object(envelope))
        .map_err(|err| DispatchError::CliInvocationFailed(format!("serialize envelope: {err}")))
}

pub(super) fn parse_cli_invocation_trace(
    stdout: &[u8],
    stderr: &[u8],
    exit_code: Option<i32>,
    duration_ms: u64,
    success: bool,
) -> Option<InvocationTrace> {
    let exec_result = ExecutionResult {
        success,
        timed_out: false,
        stdout: String::from_utf8_lossy(stdout).into_owned(),
        stderr: String::from_utf8_lossy(stderr).into_owned(),
        exit_code,
        duration_ms,
        output: None,
    };

    parse_and_validate_response(&exec_result)
        .map(|(_, _, trace)| trace)
        .ok()
}

/// Parse the provider's stdout as an Orbit response envelope and return the
/// object that workflow templates may consume as an activity output.
///
/// Provider CLIs commonly wrap the response in a JSON object and, in
/// Claude's case, may prefix the embedded response with explanatory prose.
/// `parse_and_validate_response` owns that wrapper traversal and envelope
/// validation, so the CLI backend must use the same boundary before exposing
/// output to later workflow steps.
pub(super) fn parse_cli_response_result(
    stdout: &[u8],
    stderr: &[u8],
    exit_code: Option<i32>,
    duration_ms: u64,
    success: bool,
) -> Result<serde_json::Map<String, Value>, String> {
    let exec_result = ExecutionResult {
        success,
        timed_out: false,
        stdout: String::from_utf8_lossy(stdout).into_owned(),
        stderr: String::from_utf8_lossy(stderr).into_owned(),
        exit_code,
        duration_ms,
        output: None,
    };
    let (envelope, status, _) =
        parse_and_validate_response(&exec_result).map_err(|error| error.to_string())?;

    match status {
        AgentResponseStatus::Success => {
            if let Some(Value::Object(result)) = envelope.result {
                Ok(result)
            } else {
                Err(
                    "Orbit response envelope status=success requires an object `result`"
                        .to_string(),
                )
            }
        }
        AgentResponseStatus::Failed => Err("Orbit response envelope status=failed".to_string()),
        AgentResponseStatus::Timeout => Err("Orbit response envelope status=timeout".to_string()),
    }
}

pub(super) fn user_prompt_from_input(input: &Value) -> Result<String, DispatchError> {
    match input {
        Value::Object(map) => match map.get("prompt") {
            Some(Value::String(text)) => Ok(text.clone()),
            Some(other) => serde_json::to_string(other).map_err(|err| {
                DispatchError::CliInvocationFailed(format!("serialize prompt: {err}"))
            }),
            None => serde_json::to_string(input).map_err(|err| {
                DispatchError::CliInvocationFailed(format!("serialize prompt: {err}"))
            }),
        },
        Value::String(text) => Ok(text.clone()),
        Value::Null => Ok(String::new()),
        other => serde_json::to_string(other)
            .map_err(|err| DispatchError::CliInvocationFailed(format!("serialize prompt: {err}"))),
    }
}

pub(in crate::activity_job) fn task_id_from_input(input: &Value) -> Option<&str> {
    fn non_empty(value: &str) -> Option<&str> {
        if value.is_empty() { None } else { Some(value) }
    }

    input
        .get("task_id")
        .and_then(Value::as_str)
        .and_then(non_empty)
        .or_else(|| {
            input
                .get("task")
                .and_then(|task| task.get("id"))
                .and_then(Value::as_str)
                .and_then(non_empty)
        })
        .or_else(|| {
            input
                .get("task_ids")
                .and_then(Value::as_array)
                .and_then(|items| items.iter().find_map(Value::as_str))
                .and_then(non_empty)
        })
}

pub(in crate::activity_job) fn task_ids_from_input(input: &Value) -> Vec<String> {
    if let Some(task_id) = input
        .get("task_id")
        .and_then(Value::as_str)
        .filter(|task_id| !task_id.is_empty())
    {
        return vec![task_id.to_string()];
    }
    if let Some(task_id) = input
        .get("task")
        .and_then(|task| task.get("id"))
        .and_then(Value::as_str)
        .filter(|task_id| !task_id.is_empty())
    {
        return vec![task_id.to_string()];
    }

    let values = input
        .get("task_ids")
        .and_then(Value::as_array)
        .filter(|values| !values.is_empty())
        .map(Vec::as_slice)
        .or_else(|| {
            input
                .get("candidates")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
        })
        .unwrap_or_default();
    let mut seen = std::collections::BTreeSet::new();
    values
        .iter()
        .filter_map(|value| {
            value
                .as_str()
                .or_else(|| value.get("task_id").and_then(Value::as_str))
        })
        .filter(|task_id| !task_id.is_empty())
        .filter(|task_id| seen.insert((*task_id).to_string()))
        .map(str::to_string)
        .collect()
}
