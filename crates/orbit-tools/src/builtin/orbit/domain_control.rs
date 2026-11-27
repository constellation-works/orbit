//! Domain controls reused by MCP clients; application code owns all effects.
use crate::{OrbitBuiltinAction, Tool, ToolContext, ToolExecutionKind};
use orbit_common::{
    OrbitError,
    protocol::tool_input::{reject_unknown_tool_fields, required_string},
};
use orbit_types::tool::ToolSchema;
use serde_json::{Value, json};

pub struct RoutineControlTool;

impl Tool for RoutineControlTool {
    fn execution_kind(&self) -> ToolExecutionKind {
        ToolExecutionKind::Mutating
    }
    fn schema(&self) -> ToolSchema {
        ToolSchema { name: "orbit.routine.control".into(), description: "List workspace routine status, toggle one definition using its observed enabled state and target, or replace its trigger using its observed trigger and target. Requires trusted operator authority and explicit workspace. Never replay a change after a lost reply; observe authoritative state first.".into(), builtin: true, parameters: vec![
            crate::tool::tool_param("workspace", orbit_common::governance::tool_schema::WORKSPACE_SELECTOR_DESCRIPTION, "string", true),
            crate::tool::tool_param("action", "list, toggle, or set_trigger.", "string", true),
            crate::tool::tool_param("name", "Routine name (toggle, set_trigger).", "string", false),
            crate::tool::tool_param("expected_enabled", "Observed enabled state (toggle).", "boolean", false),
            crate::tool::tool_param("enabled", "Desired enabled state (toggle).", "boolean", false),
            crate::tool::tool_param("target", "Observed target (toggle, set_trigger).", "string", false),
            crate::tool::tool_param("expected_trigger", "Observed trigger object (set_trigger).", "object", false),
            crate::tool::tool_param("trigger", "set_trigger: cron|state|deliveries_landed|condition.", "object", false),
            crate::tool::tool_param("offset", "List offset (default 0).", "integer", false),
            crate::tool::tool_param("limit", "Page size 1-50 (default 25).", "integer", false),
        ] }
    }
    fn execute(&self, ctx: &ToolContext, mut input: Value) -> Result<Value, OrbitError> {
        ensure_operator_leaf(ctx)?;
        required_string(&input, &["workspace"], "workspace")?;
        match required_string(&input, &["action"], "action")?.as_str() {
            "list" => {
                reject_unknown_tool_fields(
                    &input,
                    &["workspace", "action", "offset", "limit", "model"],
                )?;
                input
                    .as_object_mut()
                    .ok_or_else(|| OrbitError::InvalidInput("expected object".into()))?
                    .remove("action");
                input["scope"] = json!("routines");
                super::execute_host_action(ctx, input, OrbitBuiltinAction::DesktopRead)
            }
            "toggle" => {
                reject_unknown_tool_fields(
                    &input,
                    &[
                        "workspace",
                        "action",
                        "name",
                        "expected_enabled",
                        "enabled",
                        "target",
                        "model",
                    ],
                )?;
                input["kind"] = json!("routine");
                super::execute_host_action(ctx, input, OrbitBuiltinAction::DesktopAutomation)
            }
            "set_trigger" => {
                reject_unknown_tool_fields(
                    &input,
                    &[
                        "workspace",
                        "action",
                        "name",
                        "expected_trigger",
                        "trigger",
                        "target",
                        "model",
                    ],
                )?;
                input["kind"] = json!("routine");
                super::execute_host_action(ctx, input, OrbitBuiltinAction::DesktopAutomation)
            }
            _ => Err(OrbitError::InvalidInput(
                "action must be list, toggle or set_trigger".into(),
            )),
        }
    }
}

pub(super) fn ensure_operator_leaf(ctx: &ToolContext) -> Result<(), OrbitError> {
    if ctx.is_managed_run() {
        return Err(OrbitError::CapabilityDenied(
            "managed runs cannot control automation or submit catalog jobs".into(),
        ));
    }
    Ok(())
}

pub(super) fn mode_field_error(error: OrbitError, mode: &str) -> OrbitError {
    let suggestions = error.did_you_mean().unwrap_or_default().to_vec();
    OrbitError::invalid_input_with_suggestions(
        format!("{error}; not accepted with {mode}; drop it"),
        suggestions,
    )
}

/// Translate the bounded domain view to the existing shared projection implementation.
pub(super) fn bounded_read(
    ctx: &ToolContext,
    mut input: Value,
    scope: &str,
    allowed: &[&str],
    default_view_hint: &str,
) -> Result<Value, OrbitError> {
    if input.get("workspace").is_none() {
        return Err(OrbitError::InvalidInput(format!(
            "`view: bounded` requires `workspace`; pass a registered workspace name, a logical workspace ID (`ws_*`), or an absolute local checkout path; {default_view_hint}"
        )));
    }
    required_string(&input, &["workspace"], "workspace")?;
    reject_unknown_tool_fields(&input, allowed)
        .map_err(|error| mode_field_error(error, "view: bounded"))?;
    if let Some(object) = input.as_object_mut() {
        object.remove("view");
    }
    input["scope"] = json!(scope);
    super::execute_host_action(ctx, input, OrbitBuiltinAction::DesktopRead)
}
pub(super) fn bounded(input: &Value) -> Result<bool, OrbitError> {
    match input.get("view") {
        None => Ok(false),
        Some(Value::String(v)) if v == "bounded" => Ok(true),
        _ => Err(OrbitError::InvalidInput(
            "view must be bounded when supplied; the only accepted value is `bounded`".into(),
        )),
    }
}
