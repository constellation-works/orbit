use orbit_common::OrbitError;
use orbit_types::tool::{ToolParam, ToolSchema};
use serde_json::Value;

use crate::{OrbitBuiltinAction, Tool, ToolContext};

pub struct OrbitSemanticIndexTool;

impl Tool for OrbitSemanticIndexTool {
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "orbit.semantic.index".to_string(),
            description:
                "Rebuild semantic embeddings for tasks, docs, or all indexed corpora in the active workspace. Reports the sources swept from the index as stale_sources."
                    .to_string(),
            parameters: vec![
                ToolParam {
                    name: "model".to_string(),
                    description: "Optional semantic embedding model alias, such as bge-small."
                        .to_string(),
                    param_type: "string".to_string(),
                    required: false,
                },
                ToolParam {
                    name: "force".to_string(),
                    description: "Re-embed unchanged task fields.".to_string(),
                    param_type: "boolean".to_string(),
                    required: false,
                },
                ToolParam {
                    name: "kind".to_string(),
                    description: "Corpus to rebuild: tasks (default), docs, or all."
                        .to_string(),
                    param_type: "string".to_string(),
                    required: false,
                },
            ],
            builtin: true,
        }
    }

    fn execute(&self, ctx: &ToolContext, input: Value) -> Result<Value, OrbitError> {
        super::super::execute_host_action(ctx, input, OrbitBuiltinAction::SemanticIndex)
    }
}
