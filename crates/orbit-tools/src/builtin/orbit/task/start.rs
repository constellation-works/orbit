use orbit_common::OrbitError;
use orbit_types::tool::{ToolParam, ToolSchema};
use serde_json::Value;

use crate::{OrbitBuiltinAction, Tool, ToolContext};

pub struct OrbitTaskStartTool;

impl Tool for OrbitTaskStartTool {
    fn schema(&self) -> ToolSchema {
        let mut parameters = super::super::orbit_id_params("task");
        parameters.extend([
            ToolParam {
                name: "note".to_string(),
                description: "Optional lifecycle note for the start transition".to_string(),
                param_type: "string".to_string(),
                required: false,
            },
            ToolParam {
                name: "comment".to_string(),
                description: "Optional task comment to append".to_string(),
                param_type: "string".to_string(),
                required: false,
            },
            ToolParam {
                name: "crew".to_string(),
                description: "Optional crew override for this start".to_string(),
                param_type: "string".to_string(),
                required: false,
            },
        ]);
        parameters.extend(super::super::model_identity_params());

        ToolSchema {
            name: "orbit.task.start".to_string(),
            description: "Start work on an Orbit task and return the updated task JSON".to_string(),
            parameters,
            builtin: true,
        }
    }

    fn execute(&self, ctx: &ToolContext, input: Value) -> Result<Value, OrbitError> {
        super::super::reject_agent_field(&input, "orbit.task.start")?;
        super::super::reject_unknown_tool_arguments(&input, &self.schema())?;
        super::super::execute_host_action(ctx, input, OrbitBuiltinAction::TaskStart)
    }
}
