use orbit_common::OrbitError;
use orbit_common::protocol::tool_input::reject_unknown_tool_fields;
use serde_json::Value;

use orbit_common::governance::task::TaskVerb;

use crate::artifact_materialize::source_presence_error;
use crate::{OrbitBuiltinAction, ToolContext};

/// Authenticate the private path-free payload before ordinary spec parsing.
pub(super) fn preempt(ctx: &ToolContext, input: &Value) -> Option<Result<Value, OrbitError>> {
    if let Err(error) = super::super::reject_agent_field(input, "orbit.task.artifact.put") {
        return Some(Err(error));
    }
    if input.get("artifacts").is_none() {
        return source_presence_error(input).map(Err);
    }
    Some((|| {
        if (ctx.session_context.transport != Some(orbit_types::tool::McpTransport::SshMcp)
            && ctx.session_context.worker_invocation.is_none())
            || ["source_path", "sourcePath", "source-path"]
                .iter()
                .any(|key| input.get(*key).is_some())
        {
            return Err(OrbitError::InvalidInput("preloaded task artifact payloads are accepted only from the authenticated ssh-mcp spoke connector".to_string()));
        }
        reject_unknown_tool_fields(input, &["id", "artifacts", "model"])?;
        super::super::execute_host_action(
            ctx,
            input.clone(),
            OrbitBuiltinAction::Task(TaskVerb::ArtifactPut),
        )
    })())
}
