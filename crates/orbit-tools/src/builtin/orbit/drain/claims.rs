use orbit_common::OrbitError;
use orbit_types::tool::ToolSchema;
use serde_json::Value;

use crate::{OrbitBuiltinAction, Tool, ToolContext, ToolExecutionKind};

pub struct OrbitDrainClaimsTool;

impl Tool for OrbitDrainClaimsTool {
    fn execution_kind(&self) -> ToolExecutionKind {
        ToolExecutionKind::ReadOnly
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "orbit.drain.claims".to_string(),
            description:
                "List this workspace's execution claims for inspection and deliberate recovery: \
                 phase, age, reservation expiry, execution machine, bound run, last recorded \
                 event, unresolved merge intent, and landing invalidation. Age, an expired \
                 reservation, and an absent local run are diagnostics, not proof that a worker \
                 died — nothing here reclaims, rebinds, or repairs a claim."
                    .to_string(),
            parameters: Vec::new(),
            builtin: true,
        }
    }

    fn execute(&self, ctx: &ToolContext, input: Value) -> Result<Value, OrbitError> {
        super::super::execute_host_action(ctx, input, OrbitBuiltinAction::DrainClaims)
    }
}
