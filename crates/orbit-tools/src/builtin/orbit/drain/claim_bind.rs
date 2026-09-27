use orbit_common::OrbitError;
use orbit_types::tool::{ToolParam, ToolSchema};
use serde_json::Value;

use crate::{OrbitBuiltinAction, Tool, ToolContext, ToolExecutionKind};

pub struct OrbitDrainClaimBindTool;

impl Tool for OrbitDrainClaimBindTool {
    fn execution_kind(&self) -> ToolExecutionKind {
        ToolExecutionKind::Mutating
    }

    fn schema(&self) -> ToolSchema {
        let parameters = vec![
            ToolParam {
                name: "claim_id".to_string(),
                description: "The claim `orbit.task.pull` admitted to this machine.".to_string(),
                param_type: "string".to_string(),
                required: true,
            },
            ToolParam {
                name: "run_id".to_string(),
                description: "The one local leaf run the executor created for this claim."
                    .to_string(),
                param_type: "string".to_string(),
                required: true,
            },
            ToolParam {
                name: "ship".to_string(),
                description: "The ship contract the claim was admitted under, from its receipt."
                    .to_string(),
                param_type: "object".to_string(),
                required: true,
            },
        ];
        ToolSchema {
            name: "orbit.drain.claim.bind".to_string(),
            description:
                "Bind an executor's local leaf run to its execution claim on the owner, moving \
                 the claim from `claimed` to `running`. Idempotent: repeating the same run \
                 returns the recorded outcome, and a different run is refused, so one claim \
                 never has two leaves. The owner fences the claim against the session's own \
                 machine; a claim admitted to another machine, or already settled or revoked, \
                 is refused as stale."
                    .to_string(),
            parameters,
            builtin: true,
        }
    }

    fn execute(&self, ctx: &ToolContext, input: Value) -> Result<Value, OrbitError> {
        super::super::execute_host_action(ctx, input, OrbitBuiltinAction::DrainClaimBind)
    }
}
