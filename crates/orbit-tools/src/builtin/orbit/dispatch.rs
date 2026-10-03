use orbit_common::OrbitError;
use orbit_common::protocol::tool_input::reject_unknown_tool_fields;
use orbit_types::tool::{ToolParam, ToolSchema};
use serde_json::Value;

use super::identity::resolve_identity;
use crate::{OrbitBuiltinAction, ToolContext};

pub(super) fn reject_agent_field(input: &Value, tool_name: &str) -> Result<(), OrbitError> {
    if input
        .as_object()
        .is_some_and(|object| object.contains_key("agent"))
    {
        return Err(OrbitError::InvalidInput(format!(
            "{tool_name} no longer accepts `agent`; use `model` with the agent family for attribution"
        )));
    }
    Ok(())
}

pub(super) fn reject_unknown_tool_arguments(
    input: &Value,
    schema: &ToolSchema,
) -> Result<(), OrbitError> {
    let allowed = schema
        .parameters
        .iter()
        .map(|param| param.name.as_str())
        .collect::<Vec<_>>();
    reject_unknown_tool_fields(input, &allowed)
}

pub(super) fn execute_host_action(
    ctx: &ToolContext,
    input: Value,
    action: OrbitBuiltinAction,
) -> Result<Value, OrbitError> {
    let identity = resolve_identity(ctx, &input)?;
    let host = require_orbit_host(ctx)?;
    match ctx.trusted_actor_label.as_deref() {
        Some(actor_label) => host.execute_with_trusted_actor(
            action,
            input,
            actor_label.to_string(),
            ctx.reservation_owner.clone(),
        ),
        None => host.execute(
            action,
            input,
            identity.agent,
            identity.model,
            ctx.reservation_owner.clone(),
        ),
    }
}

fn require_orbit_host(ctx: &ToolContext) -> Result<&dyn crate::OrbitToolHost, OrbitError> {
    ctx.orbit_host.as_deref().ok_or_else(|| {
        OrbitError::Execution(
            "orbit builtin requires an Orbit runtime host in ToolContext".to_string(),
        )
    })
}

/// The single required `id` parameter of a `kind` lookup tool.
pub(super) fn orbit_id_params(kind: &str) -> Vec<ToolParam> {
    vec![ToolParam {
        name: "id".to_string(),
        description: format!("{kind} ID"),
        param_type: "string".to_string(),
        required: true,
    }]
}
