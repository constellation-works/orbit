use orbit_common::OrbitError;
use orbit_types::tool::{ToolParam, ToolSchema};
use serde_json::Value;

use crate::{OrbitBuiltinAction, Tool, ToolContext, ToolExecutionKind};

pub struct OrbitDrainProbeTool;

impl Tool for OrbitDrainProbeTool {
    fn execution_kind(&self) -> ToolExecutionKind {
        ToolExecutionKind::ReadOnly
    }

    fn schema(&self) -> ToolSchema {
        let parameters = vec![
            ToolParam {
                name: "caller_version".to_string(),
                description:
                    "Optional. The calling binary's version. When supplied, the response reports \
                     whether it matches this owner instead of leaving the caller to compare."
                        .to_string(),
                param_type: "string".to_string(),
                required: false,
            },
            ToolParam {
                name: "caller_schema".to_string(),
                description:
                    "Optional. The caller's distributed-drain wire-protocol schema version. This \
                     is not the MCP protocol revision and not the orchestration schema version."
                        .to_string(),
                param_type: "integer".to_string(),
                required: false,
            },
            ToolParam {
                name: "caller_before_pr".to_string(),
                description:
                    "Optional. The executor's `review.before_pr`. Diagnostic only: a claimed \
                     leaf runs the before-PR review the owner's ship contract captures, never \
                     the executor's own. After-landing review never affects admission."
                        .to_string(),
                param_type: "boolean".to_string(),
                required: false,
            },
        ];
        ToolSchema {
            name: "orbit.drain.probe".to_string(),
            description:
                "Read-only preflight for the distributed drain, served by the workspace owner. \
                 Reports the owner machine, binary version, distributed-drain protocol schema, \
                 the capabilities this session holds, the diagnostic caller machine, the \
                 owner-resolved ship configuration, and both review switches (`review.before_pr` \
                 and the after-landing auto-task). Declaring `caller_version`, \
                 `caller_schema`, or `caller_before_pr` also reports the \
                 first refusal an admission would raise, in the order admission applies it. It \
                 creates no request receipt, reservation, claim, or task transition, and it is \
                 never a health check for admission itself."
                    .to_string(),
            parameters,
            builtin: true,
        }
    }

    fn execute(&self, ctx: &ToolContext, input: Value) -> Result<Value, OrbitError> {
        super::super::execute_host_action(ctx, input, OrbitBuiltinAction::DrainProbe)
    }
}
