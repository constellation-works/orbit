use orbit_common::OrbitError;
use orbit_types::tool::{ToolParam, ToolSchema};
use serde_json::Value;

use crate::{OrbitBuiltinAction, Tool, ToolContext};

pub struct OrbitAutoTaskShowTool;

impl Tool for OrbitAutoTaskShowTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "orbit.auto_task.show".to_string(),
            description:
                "Show a single auto-task definition and persisted delivery diagnostics by name."
                    .to_string(),
            parameters: vec![
                ToolParam {
                    name: "name".to_string(),
                    description: "Definition name. Required.".to_string(),
                    param_type: "string".to_string(),
                    required: true,
                },
                ToolParam {
                    name: "preview".into(),
                    description:
                        "Preview delivery baseline/source observations without admitting actions."
                            .into(),
                    param_type: "boolean".into(),
                    required: false,
                },
            ],
            builtin: true,
        }
    }

    fn execute(&self, ctx: &ToolContext, input: Value) -> Result<Value, OrbitError> {
        super::super::execute_host_action(ctx, input, OrbitBuiltinAction::AutoTaskShow)
    }
}
