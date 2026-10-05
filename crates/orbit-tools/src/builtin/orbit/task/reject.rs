use orbit_common::OrbitError;
use orbit_types::task::TASK_SHOW_PROJECTION_FIELDS_CSV;
use orbit_types::tool::{ToolParam, ToolSchema};
use serde_json::Value;

use crate::{OrbitBuiltinAction, Tool, ToolContext};

pub struct OrbitTaskRejectTool;

impl Tool for OrbitTaskRejectTool {
    fn schema(&self) -> ToolSchema {
        let mut parameters = super::super::orbit_id_params("task");
        parameters.extend([
            ToolParam {
                name: "note".to_string(),
                description: "Required rejection note".to_string(),
                param_type: "string".to_string(),
                required: true,
            },
            ToolParam {
                name: "comment".to_string(),
                description: "Optional task comment to append".to_string(),
                param_type: "string".to_string(),
                required: false,
            },
            ToolParam {
                name: "fields".to_string(),
                description: format!(
                    "Optional response field projection as a string or array. When omitted, write responses exclude append-heavy `comments` and `history`; request those fields explicitly when needed. Valid values: {TASK_SHOW_PROJECTION_FIELDS_CSV}."
                ),
                param_type: "string_list".to_string(),
                required: false,
            },
            ToolParam {
                name: "field".to_string(),
                description: "Compatibility alias for a single response field projection, such as `field: \"history\"`.".to_string(),
                param_type: "string".to_string(),
                required: false,
            },
        ]);
        parameters.extend(super::super::identity_params());

        ToolSchema {
            name: "orbit.task.reject".to_string(),
            description: "Reject an Orbit task and return the updated task JSON".to_string(),
            parameters,
            builtin: true,
        }
    }

    fn execute(&self, ctx: &ToolContext, input: Value) -> Result<Value, OrbitError> {
        super::super::execute_host_action(ctx, input, OrbitBuiltinAction::TaskReject)
    }
}
