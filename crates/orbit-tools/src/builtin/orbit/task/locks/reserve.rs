use orbit_common::OrbitError;
use orbit_types::tool::{ToolParam, ToolSchema};
use serde_json::Value;

use crate::{OrbitBuiltinAction, Tool, ToolContext};

pub struct OrbitTaskLocksReserveTool;

impl Tool for OrbitTaskLocksReserveTool {
    fn schema(&self) -> ToolSchema {
        let mut parameters = vec![
            ToolParam {
                name: "task_ids".to_string(),
                description: "Task IDs in the bundle as a string or array of strings.".to_string(),
                param_type: "string_list".to_string(),
                required: false,
            },
            ToolParam {
                name: "files".to_string(),
                description:
                    "Canonical file or directory selectors to reserve (`file:...`, `dir:...`)."
                        .to_string(),
                param_type: "string_list".to_string(),
                required: false,
            },
            ToolParam {
                name: "ttl_seconds".to_string(),
                description: "Optional reservation TTL in seconds. Defaults to 1800; max 7200."
                    .to_string(),
                param_type: "u64".to_string(),
                required: false,
            },
        ];
        parameters.extend(super::super::super::identity_params());

        ToolSchema {
            name: "orbit.task.locks.reserve".to_string(),
            description:
                "Attempt to atomically reserve either task-derived context files (`task_ids`) or \
                 direct file/directory selectors (`files`) and return either a reservation grant \
                 or a conflict list. Exactly one of `task_ids` or `files` must be provided."
                    .to_string(),
            parameters,
            builtin: true,
        }
    }

    fn execute(&self, ctx: &ToolContext, input: Value) -> Result<Value, OrbitError> {
        super::super::super::execute_host_action(ctx, input, OrbitBuiltinAction::TaskLocksReserve)
    }
}
