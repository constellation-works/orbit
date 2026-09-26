use orbit_common::OrbitError;
use orbit_types::tool::{ToolParam, ToolSchema};
use serde_json::Value;

use crate::{OrbitBuiltinAction, Tool, ToolContext};

pub struct OrbitAutoTaskDeleteTool;

impl Tool for OrbitAutoTaskDeleteTool {
    fn schema(&self) -> ToolSchema {
        let parameters = vec![
            ToolParam {
                name: "name".to_string(),
                description: "Definition name. Required.".to_string(),
                param_type: "string".to_string(),
                required: true,
            },
            ToolParam {
                name: "reason".to_string(),
                description: "Why the definition is deleted, kept in the audit record."
                    .to_string(),
                param_type: "string".to_string(),
                required: false,
            },
            ToolParam {
                name: "force".to_string(),
                description: "Delete even while a task minted from the definition is open or a delivery action is executing. Defaults to false.".to_string(),
                param_type: "boolean".to_string(),
                required: false,
            },
        ];
        ToolSchema {
            name: "orbit.auto_task.delete".to_string(),
            description: "Delete an auto-task definition with its scheduler cursor and delivery consumer state. Deleting a shipped default records an opt-out, so reseeding does not re-create it; `orbit auto-task restore` reinstates it.".to_string(),
            parameters,
            builtin: true,
        }
    }

    fn execute(&self, ctx: &ToolContext, input: Value) -> Result<Value, OrbitError> {
        super::super::execute_host_action(ctx, input, OrbitBuiltinAction::AutoTaskDelete)
    }
}
