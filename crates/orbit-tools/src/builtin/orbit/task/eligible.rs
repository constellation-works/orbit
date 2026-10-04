use orbit_common::OrbitError;
use orbit_types::tool::{ToolParam, ToolSchema};
use serde_json::Value;

use crate::{OrbitBuiltinAction, Tool, ToolContext, ToolExecutionKind};

pub struct OrbitTaskEligibleTool;

impl Tool for OrbitTaskEligibleTool {
    fn execution_kind(&self) -> ToolExecutionKind {
        ToolExecutionKind::ReadOnly
    }

    fn schema(&self) -> ToolSchema {
        let mut parameters = vec![
            ToolParam {
                name: "status".to_string(),
                description:
                    "Candidate statuses: `backlog`, `proposed`, or both (the default). Pass a comma-separated string or an array."
                        .to_string(),
                param_type: "string_list".to_string(),
                required: false,
            },
            ToolParam {
                name: "path".to_string(),
                description:
                    "Keep only candidates whose `context_files` selectors apply to this path, matched as `orbit.task.list` matches `path`."
                        .to_string(),
                param_type: "string".to_string(),
                required: false,
            },
            ToolParam {
                name: "limit".to_string(),
                description: "Maximum eligible tasks to return (default 50). Must be at least 1."
                    .to_string(),
                param_type: "integer".to_string(),
                required: false,
            },
            ToolParam {
                name: "explain".to_string(),
                description:
                    "When true, also return `conflicting`: every held-back candidate with the overlapping selector and the in-progress or review task holding it."
                        .to_string(),
                param_type: "boolean".to_string(),
                required: false,
            },
        ];
        parameters.extend(super::super::identity_params());
        ToolSchema {
            name: "orbit.task.eligible".to_string(),
            description:
                "List backlog and proposed tasks that can be picked up now without colliding with work in flight, as {tasks, total, truncated[, conflicting]}. A task is eligible when its context-file lock surface overlaps no in-progress or review task's surface, decided by the same lock code automatic dispatch uses. No other gate applies: dependencies, complexity, groups, crew and overlap between candidates are not checked. Read-only."
                    .to_string(),
            parameters,
            builtin: true,
        }
    }

    fn execute(&self, ctx: &ToolContext, input: Value) -> Result<Value, OrbitError> {
        super::super::reject_unknown_tool_arguments(&input, &self.schema())?;
        super::super::execute_host_action(ctx, input, OrbitBuiltinAction::TaskEligible)
    }
}
