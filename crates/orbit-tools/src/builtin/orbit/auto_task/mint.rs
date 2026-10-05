use orbit_common::OrbitError;
use orbit_types::tool::{ToolParam, ToolSchema};
use serde_json::{Value, json};

use crate::{OrbitBuiltinAction, Tool, ToolContext};

pub struct OrbitAutoTaskMintTool;

impl Tool for OrbitAutoTaskMintTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "orbit.auto_task.mint".to_string(),
            description: "Mint one task now from an auto-task definition. Unconditional: the schedule, dedupe policy, and enabled flag are ignored, and the scheduler's own cursor is left untouched.".to_string(),
            parameters: vec![ToolParam {
                name: "name".to_string(),
                description: "Definition name. Required.".to_string(),
                param_type: "string".to_string(),
                required: true,
            }, super::super::task::guarded::param("workspace", "string", "Explicit workspace for acknowledged mint"), super::super::task::guarded::param("acknowledge_unconditional", "boolean", "True acknowledges unconditional creation; requires operator authority")],
            builtin: true,
        }
    }

    fn execute(&self, ctx: &ToolContext, mut input: Value) -> Result<Value, OrbitError> {
        if input.get("acknowledge_unconditional").is_some() {
            super::super::domain_control::ensure_operator_leaf(ctx)?;
            super::super::reject_unknown_tool_arguments(&input, &self.schema())?;
            orbit_common::protocol::tool_input::required_string(
                &input,
                &["workspace"],
                "workspace",
            )?;
            input["action"] = json!("mint");
            input["kind"] = json!("auto_task");
            return super::super::execute_host_action(
                ctx,
                input,
                OrbitBuiltinAction::DesktopAutomation,
            );
        }
        super::super::execute_host_action(ctx, input, OrbitBuiltinAction::AutoTaskMint)
    }
}
