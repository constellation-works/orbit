use orbit_common::OrbitError;
use orbit_types::tool::{ToolParam, ToolSchema};
use serde_json::Value;

use crate::{OrbitBuiltinAction, Tool, ToolContext};

pub struct OrbitAutoTaskListTool;

impl Tool for OrbitAutoTaskListTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "orbit.auto_task.list".to_string(),
            description: "List auto-task definitions in this workspace. Definitions seeded by \
                          a plugin that is switched off here are omitted unless \
                          include_inactive_plugins is true."
                .to_string(),
            parameters: vec![ToolParam {
                name: "include_inactive_plugins".to_string(),
                description: "Also list definitions whose seeding plugin is off in this \
                              workspace or on the host, each marked plugin_inactive with its \
                              skipped_reason."
                    .to_string(),
                param_type: "boolean".to_string(),
                required: false,
            }],
            builtin: true,
        }
    }

    fn execute(&self, ctx: &ToolContext, input: Value) -> Result<Value, OrbitError> {
        super::super::execute_host_action(ctx, input, OrbitBuiltinAction::AutoTaskList)
    }
}
