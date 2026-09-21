use orbit_common::OrbitError;
use orbit_types::tool::{ToolParam, ToolSchema};
use serde_json::Value;

use crate::{OrbitBuiltinAction, Tool, ToolContext, ToolExecutionKind};

pub struct OrbitSearchTool;

impl Tool for OrbitSearchTool {
    fn execution_kind(&self) -> ToolExecutionKind {
        ToolExecutionKind::ReadOnly
    }

    fn schema(&self) -> ToolSchema {
        let mut parameters = vec![
            ToolParam {
                name: "query".to_string(),
                description: "Free-text query. Defaults to lexical matching unless hybrid is true."
                    .to_string(),
                param_type: "string".to_string(),
                required: false,
            },
            ToolParam {
                // ADR-0179: expose the free-text vector ranker as hybrid, not semantic.
                name: "hybrid".to_string(),
                description:
                    "Opt into hybrid lexical + cosine ranking for indexed task vectors; frictions remain lexical."
                        .to_string(),
                param_type: "boolean".to_string(),
                required: false,
            },
            ToolParam {
                // ADR-0179: semantic carries the task ID for cosine-neighbor lookup on MCP.
                name: "semantic".to_string(),
                description:
                    "Task ID for cosine-neighbor lookup. Mutually exclusive with query."
                        .to_string(),
                param_type: "string".to_string(),
                required: false,
            },
            ToolParam {
                name: "kind".to_string(),
                description: "Corpus kind: task, friction, or all. Default: all."
                    .to_string(),
                param_type: "string".to_string(),
                required: false,
            },
            ToolParam {
                name: "limit".to_string(),
                description: "Maximum number of results. Default: 10.".to_string(),
                param_type: "integer".to_string(),
                required: false,
            },
            ToolParam {
                name: "tag".to_string(),
                description:
                    "AND-filter by tag. Repeat or pass an array. Applies to task and friction."
                        .to_string(),
                param_type: "string_list".to_string(),
                required: false,
            },
            ToolParam {
                name: "all".to_string(),
                description:
                    "Include normally-hidden statuses for the queried kind. Task adds done/rejected/archived; friction adds triaged/resolved."
                        .to_string(),
                param_type: "boolean".to_string(),
                required: false,
            },
            ToolParam {
                name: "status".to_string(),
                description:
                    "Explicit per-kind status override using kind:value tokens, such as task:open or friction:open. Overrides `all` for the named kind."
                        .to_string(),
                param_type: "string_list".to_string(),
                required: false,
            },
            ToolParam {
                // ORB-11027: the federated scope, deliberately not named
                // `workspace` — that is the reserved routing selector that
                // binds a call to one registered checkout.
                name: "workspaces".to_string(),
                description:
                    "Federated scope: registered workspace names, `ws_*` IDs, or absolute checkout paths to search together. Hits carry workspace attribution. Omit to search only the bound workspace."
                        .to_string(),
                param_type: "string_list".to_string(),
                required: false,
            },
            ToolParam {
                name: "all_workspaces".to_string(),
                description:
                    "Search every active workspace registered on the answering machine. Overrides `workspaces`. Not available inside an Orbit-managed run."
                        .to_string(),
                param_type: "boolean".to_string(),
                required: false,
            },
        ];
        parameters.extend(super::model_identity_params());
        ToolSchema {
            name: "orbit.search".to_string(),
            description:
                "Search tasks and frictions; hybrid vector ranking applies to indexed tasks."
                    .to_string(),
            parameters,
            builtin: true,
        }
    }

    fn execute(&self, ctx: &ToolContext, input: Value) -> Result<Value, OrbitError> {
        super::execute_host_action(ctx, input, OrbitBuiltinAction::Search)
    }
}
