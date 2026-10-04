//! Domain controls reused by MCP clients; application code owns all effects.
use crate::{OrbitBuiltinAction, Tool, ToolContext, ToolExecutionKind};
use orbit_common::{
    OrbitError,
    protocol::tool_input::{reject_unknown_tool_fields, required_string},
};
use orbit_types::tool::{ToolParam, ToolSchema};
use serde_json::{Value, json};

pub struct WorkflowAutoTool;
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
impl Tool for WorkflowAutoTool {
    fn execution_kind(&self) -> ToolExecutionKind {
        ToolExecutionKind::Mutating
    }
    fn schema(&self) -> ToolSchema {
        let mut parameters = vec![
            parameter("action", "string", true, "status, start, stop or resize"),
            parameter(
                "for_seconds",
                "integer",
                false,
                "Required for start: window length in seconds, 1 through 86400 (24 hours)",
            ),
            parameter(
                "concurrency",
                "integer",
                false,
                "Worker limit, positive u32. Optional for start (omitted uses runtime default); required for resize. The drain's only ceiling: delivery jobs impose none of their own",
            ),
            parameter(
                "id",
                "string",
                false,
                "Resize only: auto- or pull-drain run ID. Omitted targets the workspace's one live drain; refused when none or several are live",
            ),
            parameter(
                "if_revision",
                "integer",
                false,
                "Resize only: apply only if the run's worker limit is still at this revision, so a concurrent change is reported rather than overwritten",
            ),
            parameter(
                "reason",
                "string",
                false,
                "Resize only: note recorded with the change",
            ),
            parameter(
                "complete",
                "boolean",
                false,
                "Start only: explicitly authorize automatic completion for all tasks admitted by this window; default false keeps review",
            ),
            parameter(
                "claim_token",
                "string",
                false,
                "Workspace claim token when held by another operator",
            ),
            parameter(
                "workspace",
                "string",
                true,
                "Exact selector returned by workspace discovery. No implicit destination.",
            ),
        ];
        parameters.extend(super::model_identity_params());
        ToolSchema {
            name: "orbit.workflow.auto".into(),
            description: "Observe workspace auto-drain readiness, start a bounded window, stop admissions while preserving admitted workers, or resize a running drain's worker limit. Resize keeps the run ID, deadline, completion authorization and dispatched children; a lower limit only stops new admissions and cancels nothing. Requires trusted operator authority and an explicit workspace. Start is not retry-safe: reconcile readiness and runs after a lost reply before another submission.".into(),
            parameters,
            builtin: true,
        }
    }
    fn execute(&self, ctx: &ToolContext, mut input: Value) -> Result<Value, OrbitError> {
        ensure_operator_leaf(ctx)?;
        super::reject_unknown_tool_arguments(&input, &self.schema())?;
        required_string(&input, &["workspace"], "workspace")?;
        if required_string(&input, &["action"], "action")? == "status" {
            reject_unknown_tool_fields(&input, &["workspace", "action", "model"])?;
            input
                .as_object_mut()
                .ok_or_else(|| OrbitError::InvalidInput("expected object".into()))?
                .remove("action");
            input["scope"] = json!("drain");
            super::execute_host_action(ctx, input, OrbitBuiltinAction::DesktopRead)
        } else {
            super::execute_host_action(ctx, input, OrbitBuiltinAction::DesktopDrain)
        }
    }
}
impl Tool for RoutineControlTool {
    fn execution_kind(&self) -> ToolExecutionKind {
        ToolExecutionKind::Mutating
    }
    fn schema(&self) -> ToolSchema {
        ToolSchema { name: "orbit.routine.control".into(), description: "List workspace routine status or toggle one definition using its observed enabled state and target. Requires trusted operator authority and explicit workspace. Never replay a toggle after a lost reply; observe authoritative state first.".into(), builtin: true, parameters: vec![
            parameter("workspace", "string", true, "Exact workspace selector from discovery"),
            parameter("action", "string", true, "list or toggle"),
            parameter("name", "string", false, "Required for toggle: exact routine name"),
            parameter("expected_enabled", "boolean", false, "Required for toggle: observed enabled state"),
            parameter("enabled", "boolean", false, "Required for toggle: desired enabled state"),
            parameter("target", "string", false, "Required for toggle: observed routine target"),
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
            _ => Err(OrbitError::InvalidInput(
                "action must be list or toggle".into(),
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
pub(super) fn bounded_params(names: &[(&str, &str, &str)]) -> Vec<ToolParam> {
    std::iter::once(parameter("view", "string", false, "bounded: bounded metadata, explicit workspace, pagination and display truncation. Omit for the existing full-record response."))
        .chain(names.iter().map(|(name, ty, description)| parameter(name, ty, false, description))).collect()
}
