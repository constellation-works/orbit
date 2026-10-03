use orbit_common::OrbitError;
use orbit_types::tool::{ToolParam, ToolSchema};
use serde_json::{Value, json};

use crate::{OrbitBuiltinAction, Tool, ToolContext};

pub struct OrbitAutoTaskToggleTool;

impl Tool for OrbitAutoTaskToggleTool {
    fn schema(&self) -> ToolSchema {
        let mut parameters = vec![
            ToolParam {
                name: "name".to_string(),
                description: "Definition name. Required.".to_string(),
                param_type: "string".to_string(),
                required: true,
            },
            ToolParam {
                name: "enabled".to_string(),
                description: "Whether to enable (`true`) or disable (`false`) the definition. Disabling is the kill-switch, not a delete. Required.".to_string(),
                param_type: "boolean".to_string(),
                required: true,
            },
        ];
        parameters.extend([
            super::super::task::guarded::param(
                "workspace",
                "string",
                "Explicit workspace for checked toggle",
            ),
            super::super::task::guarded::param(
                "expected_enabled",
                "boolean",
                "Observed enabled state checked atomically; requires operator authority",
            ),
        ]);
        ToolSchema {
            name: "orbit.auto_task.toggle".to_string(),
            description: "Enable or disable an auto-task definition without deleting it."
                .to_string(),
            parameters,
            builtin: true,
        }
    }

    fn execute(&self, ctx: &ToolContext, mut input: Value) -> Result<Value, OrbitError> {
        if input.get("expected_enabled").is_some() {
            super::super::domain_control::ensure_operator_leaf(ctx)?;
            super::super::reject_unknown_tool_arguments(&input, &self.schema())?;
            orbit_common::protocol::tool_input::required_string(
                &input,
                &["workspace"],
                "workspace",
            )?;
            input["action"] = json!("toggle");
            input["kind"] = json!("auto_task");
            return super::super::execute_host_action(
                ctx,
                input,
                OrbitBuiltinAction::DesktopAutomation,
            );
        }
        super::super::execute_host_action(ctx, input, OrbitBuiltinAction::AutoTaskToggle)
    }
}
