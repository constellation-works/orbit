//! Domain controls reused by MCP clients; application code owns all effects.
use crate::{OrbitBuiltinAction, Tool, ToolContext, ToolExecutionKind};
use orbit_common::{
    OrbitError,
    protocol::tool_input::{reject_unknown_tool_fields, required_string},
};
use orbit_types::tool::{ToolParam, ToolSchema};
use serde_json::{Value, json};

pub struct RoutineControlTool;
fn parameter(name: &str, ty: &str, required: bool, description: &str) -> ToolParam {
    ToolParam {
        name: name.into(),
        param_type: ty.into(),
        required,
        description: description.into(),
    }
}
pub(super) fn ensure_operator_leaf(ctx: &ToolContext) -> Result<(), OrbitError> {
    if ctx
        .orbit_host
        .as_ref()
        .is_some_and(|host| host.task_scope().run_id.is_some())
    {
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
            parameter("workspace", "string", true, "Exact workspace selector from discovery"),
            parameter("action", "string", true, "list, toggle or set_trigger"),
            parameter("name", "string", false, "Required for toggle and set_trigger: exact routine name"),
            parameter("expected_enabled", "boolean", false, "Required for toggle: observed enabled state"),
            parameter("enabled", "boolean", false, "Required for toggle: desired enabled state"),
            parameter("target", "string", false, "Required for toggle and set_trigger: observed routine target"),
            parameter("expected_trigger", "object", false, "Required for set_trigger: observed trigger object, as listed"),
            parameter("trigger", "object", false, "Required for set_trigger: replacement trigger, exactly one of `cron` (with optional `missed_run`), `state` or `deliveries_landed`"),
            parameter("offset", "integer", false, "List offset; default 0"),
            parameter("limit", "integer", false, "List size 1 through 50; default 25"),
            parameter("model", "string", false, "Diagnostic model attribution; never grants authority"),
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
) -> Result<Value, OrbitError> {
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
            "view must be bounded when supplied".into(),
        )),
    }
}
