//! Domain controls reused by MCP clients; application code owns all effects.
use crate::{OrbitBuiltinAction, Tool, ToolContext, ToolExecutionKind};
use orbit_common::{
    OrbitError,
    protocol::tool_input::{reject_unknown_tool_fields, required_string},
};
use orbit_types::tool::ToolSchema;
use serde_json::{Value, json};

pub struct RoutineControlTool;
pub(super) fn ensure_operator_leaf(ctx: &ToolContext) -> Result<(), OrbitError> {
    if ctx.is_managed_run() {
        return Err(OrbitError::CapabilityDenied(
            "managed runs cannot control automation or submit catalog jobs".into(),
        ));
    }
    Ok(())
}
impl Tool for RoutineControlTool {
    fn execution_kind(&self) -> ToolExecutionKind {
        ToolExecutionKind::Mutating
    }
    fn schema(&self) -> ToolSchema {
        ToolSchema { name: "orbit.routine.control".into(), description: "List workspace routine status, toggle one definition using its observed enabled state and target, or replace its trigger using its observed trigger and target. Requires trusted operator authority and explicit workspace. Never replay a change after a lost reply; observe authoritative state first.".into(), builtin: true, parameters: vec![
            crate::tool::tool_param("workspace", "Exact workspace selector from discovery", "string", true),
            crate::tool::tool_param("action", "list, toggle or set_trigger", "string", true),
            crate::tool::tool_param("name", "Required for toggle and set_trigger: exact routine name", "string", false),
            crate::tool::tool_param("expected_enabled", "Required for toggle: observed enabled state", "boolean", false),
            crate::tool::tool_param("enabled", "Required for toggle: desired enabled state", "boolean", false),
            crate::tool::tool_param("target", "Required for toggle and set_trigger: observed routine target", "string", false),
            crate::tool::tool_param("expected_trigger", "Required for set_trigger: observed trigger object, as listed", "object", false),
            crate::tool::tool_param("trigger", "Required for set_trigger: replacement trigger, exactly one of `cron` (with optional `missed_run`), `state`, `deliveries_landed` or `condition` (checked and explained but never fired in this release; routine must stay disabled)", "object", false),
            crate::tool::tool_param("offset", "List offset; default 0", "integer", false),
            crate::tool::tool_param("limit", "List size 1 through 50; default 25", "integer", false),
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
    reject_unknown_tool_fields(&input, allowed)?;
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
