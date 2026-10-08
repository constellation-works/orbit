use orbit_common::OrbitError;
use orbit_common::protocol::tool_input::{optional_string, optional_u32_alias, required_string};
use orbit_types::identity::normalize_optional_attribution_label;
use serde_json::Value;

use crate::OrbitRuntime;
use crate::runtime::command_exec::RemoteCommandParams;

pub(super) fn exec(
    runtime: &OrbitRuntime,
    input: Value,
    agent: Option<String>,
    model: Option<String>,
) -> Result<Value, OrbitError> {
    let argv = required_argv(&input)?;
    let working_directory = required_string(&input, &["working_directory"], "working_directory")?;
    let claim_token = optional_string(&input, "claim_token")?;
    let timeout_ms = optional_u32_alias(&input, &["timeout_ms"])?.map(u64::from);
    if timeout_ms == Some(0) {
        return Err(OrbitError::InvalidInput(
            "`timeout_ms` must be at least 1".to_string(),
        ));
    }
    let actor = normalize_optional_attribution_label(
        model.as_deref().or(agent.as_deref()),
        model.as_deref(),
    )
    .unwrap_or_else(|| runtime.actor_label().to_string());

    let result = runtime.execute_remote_command(RemoteCommandParams {
        argv,
        working_directory,
        claim_token,
        actor,
        timeout_ms,
    })?;

    serde_json::to_value(result)
        .map_err(|error| OrbitError::Execution(format!("serialize command exec result: {error}")))
}

/// Reject anything but a non-empty JSON array of non-empty strings. A shell
/// string (`argv: "ls -la"`) is the exact mistake this rejects: it would spawn
/// a single, almost certainly nonexistent program named `"ls -la"` rather than
/// silently gaining shell semantics, but the explicit rejection here means the
/// caller learns why immediately instead of chasing a spawn failure.
fn required_argv(input: &Value) -> Result<Vec<String>, OrbitError> {
    match input.get("argv") {
        None | Some(Value::Null) => Err(OrbitError::InvalidInput(
            "missing `argv`; pass the command as a JSON array of argument strings, e.g. \
             [\"git\", \"status\"]"
                .to_string(),
        )),
        Some(Value::String(_)) => Err(OrbitError::InvalidInput(
            "`argv` must be a JSON array of argument strings, never a shell string; command \
             execution never interprets shell syntax"
                .to_string(),
        )),
        Some(Value::Array(items)) => {
            if items.is_empty() {
                return Err(OrbitError::InvalidInput(
                    "`argv` must not be empty; it must name at least the program to run"
                        .to_string(),
                ));
            }
            items
                .iter()
                .map(|item| {
                    item.as_str()
                        .filter(|value| !value.is_empty())
                        .map(str::to_string)
                        .ok_or_else(|| {
                            OrbitError::InvalidInput(
                                "`argv` entries must be non-empty strings".to_string(),
                            )
                        })
                })
                .collect()
        }
        Some(_) => Err(OrbitError::InvalidInput(
            "`argv` must be a JSON array of argument strings".to_string(),
        )),
    }
}
