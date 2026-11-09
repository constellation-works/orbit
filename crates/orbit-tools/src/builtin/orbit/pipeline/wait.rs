use orbit_common::OrbitError;
use orbit_types::tool::{ToolParam, ToolSchema};
use serde_json::Value;

use crate::{OrbitBuiltinAction, Tool, ToolContext};

pub struct OrbitPipelineWaitTool;

impl Tool for OrbitPipelineWaitTool {
    fn schema(&self) -> ToolSchema {
        let mut parameters = vec![
            ToolParam {
                name: "run_ids".to_string(),
                description: "One or more run IDs to wait for as a string or array of strings."
                    .to_string(),
                param_type: "string_list".to_string(),
                required: true,
            },
            ToolParam {
                name: "timeout_seconds".to_string(),
                description: "Optional timeout in seconds (default 3600, max 86400).".to_string(),
                param_type: "number".to_string(),
                required: false,
            },
            ToolParam {
                name: "poll_interval_seconds".to_string(),
                description:
                    "Optional poll cadence in seconds (default 5, clamped to a 1-second floor)."
                        .to_string(),
                param_type: "number".to_string(),
                required: false,
            },
        ];
        parameters.extend(super::super::identity_params());

        ToolSchema {
            name: "orbit.pipeline.wait".to_string(),
            description: "Block until the supplied pipeline runs reach terminal state or timeout."
                .to_string(),
            parameters,
            builtin: true,
        }
    }

    fn execute(&self, ctx: &ToolContext, input: Value) -> Result<Value, OrbitError> {
        super::super::execute_host_action(ctx, input, OrbitBuiltinAction::PipelineWait)
    }
}
