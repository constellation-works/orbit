use orbit_common::OrbitError;
use orbit_types::tool::{ToolParam, ToolSchema};
use serde_json::Value;

use crate::{OrbitBuiltinAction, Tool, ToolContext, ToolExecutionKind};

pub struct OrbitDrainClaimSettleTool;

impl Tool for OrbitDrainClaimSettleTool {
    fn execution_kind(&self) -> ToolExecutionKind {
        ToolExecutionKind::Mutating
    }

    fn schema(&self) -> ToolSchema {
        let parameters = vec![
            ToolParam {
                name: "claim_id".to_string(),
                description: "The claim being settled.".to_string(),
                param_type: "string".to_string(),
                required: true,
            },
            ToolParam {
                name: "run_id".to_string(),
                description: "The leaf run bound to this claim, when one was bound.".to_string(),
                param_type: "string".to_string(),
                required: false,
            },
            ToolParam {
                name: "settlement".to_string(),
                description:
                    "The executor's durable settlement: `{\"AcceptHandoff\": <typed handoff>}` \
                     for a published pull request, or `{\"Fail\": <evidence>}`. Nothing else \
                     settles a claim."
                        .to_string(),
                param_type: "object".to_string(),
                required: true,
            },
        ];
        ToolSchema {
            name: "orbit.drain.claim.settle".to_string(),
            description:
                "Settle an execution claim on the owner. A typed handoff is accepted only after \
                 the owner reads the pull request from the provider and resolves its candidate \
                 and base in the owner's own checkout, and only with digest-pinned validation \
                 evidence; it moves the task to `review`, which awaits owner approval — it does \
                 not merge. A failure records its evidence and blocks the task. Either releases \
                 only this claim's reservation. Retries replay the recorded outcome."
                    .to_string(),
            parameters,
            builtin: true,
        }
    }

    fn execute(&self, ctx: &ToolContext, input: Value) -> Result<Value, OrbitError> {
        super::super::execute_host_action(ctx, input, OrbitBuiltinAction::DrainClaimSettle)
    }
}
