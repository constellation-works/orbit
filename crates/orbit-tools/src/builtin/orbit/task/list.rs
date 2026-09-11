use orbit_common::OrbitError;
use orbit_types::tool::{ToolParam, ToolSchema};
use serde_json::Value;

use crate::{OrbitBuiltinAction, Tool, ToolContext, ToolExecutionKind};

pub struct OrbitTaskListTool;

impl Tool for OrbitTaskListTool {
    fn execution_kind(&self) -> ToolExecutionKind {
        ToolExecutionKind::ReadOnly
    }

    fn schema(&self) -> ToolSchema {
        let mut parameters = vec![
            ToolParam {
                name: "status".to_string(),
                description:
                    "Optional task status filter. Pass a comma-separated string or an array of status names."
                        .to_string(),
                param_type: "string_list".to_string(),
                required: false,
            },
            ToolParam {
                name: "parent_id".to_string(),
                description: "Optional parent task ID to list subtasks for".to_string(),
                param_type: "string".to_string(),
                required: false,
            },
            ToolParam {
                name: "type".to_string(),
                description: "Optional task type filter".to_string(),
                param_type: "string".to_string(),
                required: false,
            },
            ToolParam {
                name: "job_run_id".to_string(),
                description: "Filter by job run ID".to_string(),
                param_type: "string".to_string(),
                required: false,
            },
            ToolParam {
                name: "tag".to_string(),
                description: "Filter by tag. Repeat or pass an array for AND semantics."
                    .to_string(),
                param_type: "string_list".to_string(),
                required: false,
            },
            ToolParam {
                name: "ready".to_string(),
                description: "When true, keep only tasks whose dependencies are satisfied"
                    .to_string(),
                param_type: "boolean".to_string(),
                required: false,
            },
            ToolParam {
                name: "path".to_string(),
                description:
                    "Filter to tasks whose `context_files` selectors apply to this path. Supports file:, dir:, symbol:, and bare path selectors; bidirectional containment."
                        .to_string(),
                param_type: "string".to_string(),
                required: false,
            },
            ToolParam {
                name: "limit".to_string(),
                description:
                    "Maximum number of tasks to return (default 50). With no status filter, non-terminal tasks come first and terminal tasks follow, each newest first. Must be at least 1."
                        .to_string(),
                param_type: "integer".to_string(),
                required: false,
            },
        ];
        parameters.extend(super::super::identity_params());
        ToolSchema {
            name: "orbit.task.list".to_string(),
            description:
                "List Orbit tasks in a {tasks, total, truncated} envelope (default limit 50). With no status filter, non-terminal tasks come first and terminal tasks follow, each newest first; an explicit status filter is newest first. `total` counts all matching tasks and `truncated` reports whether the limit omitted tasks. Optional filters: status, parent, type, tag, dependency readiness, path."
                .to_string(),
            parameters,
            builtin: true,
        }
    }

    fn execute(&self, ctx: &ToolContext, input: Value) -> Result<Value, OrbitError> {
        super::super::execute_host_action(ctx, input, OrbitBuiltinAction::TaskList)
    }
}
