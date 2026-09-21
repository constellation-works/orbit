use orbit_common::OrbitError;
use orbit_types::task::TASK_SHOW_PROJECTION_FIELDS_CSV;
use orbit_types::tool::{ToolParam, ToolSchema};
use serde_json::Value;

use crate::{OrbitBuiltinAction, Tool, ToolContext, ToolExecutionKind};

pub struct OrbitTaskShowTool;

impl Tool for OrbitTaskShowTool {
    fn execution_kind(&self) -> ToolExecutionKind {
        ToolExecutionKind::ReadOnly
    }

    fn schema(&self) -> ToolSchema {
        let mut parameters = vec![ToolParam {
            name: "id".to_string(),
            description: "Globally unique task ID. Resolved through the host task registry by \
                default; a workspace argument is not required."
                .to_string(),
            param_type: "string".to_string(),
            required: true,
        }];
        parameters.extend(super::super::identity_params());
        parameters.push(ToolParam {
            name: "fields".to_string(),
            description: format!(
                "Optional field projection as a string or array of strings. When set, returns only \
                the requested field(s) as JSON. A single derived `terminal` \
                selection remains keyed as an object. Valid values: \
                {TASK_SHOW_PROJECTION_FIELDS_CSV}. \
                `crew` is execution selection; `orchestrator` is separate orchestration attribution."
            ),
            param_type: "string_list".to_string(),
            required: false,
        });
        parameters.push(ToolParam {
            name: "field".to_string(),
            description:
                "Compatibility alias for a single field projection. Example: `field: \"artifacts\"`."
                    .to_string(),
            param_type: "string".to_string(),
            required: false,
        });
        parameters.push(ToolParam {
            name: "workspace".to_string(),
            description:
                "Optional explicit workspace filter. `id` is resolved globally by default; do not \
                pass cwd, MCP session/initialize metadata, or a linked-worktree runtime identity \
                (for example `orbit-5c61b3`). When supplied, a registered workspace name, logical \
                workspace ID (`ws_*`), or absolute local checkout path is fail-closed: a valid \
                workspace that does not own the task returns not-found, and an unknown selector \
                is rejected by name."
                    .to_string(),
            param_type: "string".to_string(),
            required: false,
        });
        ToolSchema {
            name: "orbit.task.show".to_string(),
            description: "Fetch a single Orbit task as JSON. `id` is a globally unique primary \
                key resolved through the host task registry by default; cwd, MCP initialize \
                metadata, and linked-worktree runtime identities are not used as filters. An \
                optional `workspace` argument is an explicit fail-closed filter only. Use the \
                optional `fields` projection (or single-field alias `field`) to retrieve only \
                specific task fields, including the derived read-only `terminal` field. \
                The `crew` field \
                selects execution, while `orchestrator` records orchestration attribution."
                .to_string(),
            parameters,
            builtin: true,
        }
    }

    fn execute(&self, ctx: &ToolContext, input: Value) -> Result<Value, OrbitError> {
        super::super::execute_host_action(ctx, input, OrbitBuiltinAction::TaskShow)
    }
}
