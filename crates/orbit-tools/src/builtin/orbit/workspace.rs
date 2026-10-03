use orbit_common::OrbitError;
use orbit_common::protocol::tool_input::optional_string_alias;
use serde_json::Value;

use crate::ToolContext;

pub(super) fn resolve_workspace_argument(
    ctx: &ToolContext,
    input: &mut Value,
    tool_name: &str,
) -> Result<String, OrbitError> {
    // MCP workspace defaults come from explicit session context, never process cwd.
    // CLI `orbit tool run` binds the runtime through RegisteredRuntimeFactory;
    // individual tools must not repeat workspace resolution.
    let explicit = optional_string_alias(input, &["workspace"])?;
    let session = ctx
        .session_context
        .workspace
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);

    match (explicit, session) {
        (Some(workspace), Some(session_workspace)) => {
            if workspace != session_workspace {
                tracing::info!(
                    target: "orbit.tools.workspace",
                    tool_name,
                    explicit_workspace = %workspace,
                    session_workspace = %session_workspace,
                    "explicit workspace overrides MCP session context"
                );
            }
            set_input_workspace(input, &workspace)?;
            Ok(workspace)
        }
        (Some(workspace), None) => {
            set_input_workspace(input, &workspace)?;
            Ok(workspace)
        }
        (None, Some(workspace)) => {
            set_input_workspace(input, &workspace)?;
            Ok(workspace)
        }
        (None, None) => Err(OrbitError::InvalidInput(
            "missing `workspace`; pass a registered workspace name, a logical workspace ID \
             (`ws_*`), or an absolute local checkout path, or initialize the MCP session with \
             `_meta.orbit.workspace`"
                .to_string(),
        )),
    }
}

/// Apply the MCP session's configured orchestrator crew to a task-creation
/// call that did not name one [ORB-11313].
///
/// Creation only. Attribution on an existing task is never rewritten from
/// ambient session configuration, so `orbit.task.update` deliberately does
/// not call this and existing records are never backfilled.
///
/// An explicit `orchestrator` on the call always wins; JSON `null` counts as
/// absent, matching how every other optional field reads. The resolved name
/// is written into the input rather than checked here: only the target
/// workspace's crew registry knows which crews exist, and it rejects an
/// unknown name instead of falling back to another crew.
pub(super) fn apply_session_orchestrator_default(ctx: &ToolContext, input: &mut Value) {
    if input
        .get("orchestrator")
        .is_some_and(|value| !value.is_null())
    {
        return;
    }
    let Some(session_orchestrator) = ctx
        .session_context
        .orchestrator
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return;
    };
    let Some(object) = input.as_object_mut() else {
        return;
    };
    object.insert(
        "orchestrator".to_string(),
        Value::String(session_orchestrator.to_string()),
    );
}

fn set_input_workspace(input: &mut Value, workspace: &str) -> Result<(), OrbitError> {
    let Some(object) = input.as_object_mut() else {
        return Err(OrbitError::InvalidInput(
            "tool input must be a JSON object".to_string(),
        ));
    };
    object.insert(
        "workspace".to_string(),
        Value::String(workspace.to_string()),
    );
    Ok(())
}
