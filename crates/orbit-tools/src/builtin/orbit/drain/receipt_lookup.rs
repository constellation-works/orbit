use orbit_common::OrbitError;
use orbit_types::tool::{ToolParam, ToolSchema};
use serde_json::Value;

use crate::{OrbitBuiltinAction, Tool, ToolContext, ToolExecutionKind};

pub struct OrbitDrainReceiptLookupTool;

impl Tool for OrbitDrainReceiptLookupTool {
    fn execution_kind(&self) -> ToolExecutionKind {
        ToolExecutionKind::ReadOnly
    }

    fn schema(&self) -> ToolSchema {
        let parameters = vec![
            ToolParam {
                name: "request_id".to_string(),
                description:
                    "The durable request ID the original admission was made with. It is read \
                     verbatim; the stored receipt is never rewritten to match the caller's \
                     current binary."
                        .to_string(),
                param_type: "string".to_string(),
                required: true,
            },
            ToolParam {
                name: "machine_id".to_string(),
                description:
                    "Optional receipt namespace: the machine the original request was made from. \
                     Defaults to this session's own machine. Naming another machine is \
                     cross-attempt inspection and requires operator capability."
                        .to_string(),
                param_type: "string".to_string(),
                required: false,
            },
            ToolParam {
                name: "lookup_schema".to_string(),
                description:
                    "Optional lookup protocol version; defaults to 1. Versioned independently of \
                     admission, so an upgraded client can reconcile an old request. An \
                     incompatible version is refused — reconcile through owner claim inspection \
                     instead of minting a replacement request."
                        .to_string(),
                param_type: "integer".to_string(),
                required: false,
            },
        ];
        ToolSchema {
            name: "orbit.drain.receipt.lookup".to_string(),
            description:
                "Read-only reconciliation of one distributed-drain admission request, served by \
                 the workspace owner. Returns the original receipt and the current claim phase \
                 (`found`), a non-reusable tombstone (`expired`), or `not_found`. It creates no \
                 receipt, binds no run, and grants no execution authority: a receipt is \
                 historical evidence, and `not_found` is not proof that an earlier request \
                 cannot still arrive, so it never licenses a replacement request under a new ID."
                    .to_string(),
            parameters,
            builtin: true,
        }
    }

    fn execute(&self, ctx: &ToolContext, input: Value) -> Result<Value, OrbitError> {
        super::super::execute_host_action(ctx, input, OrbitBuiltinAction::DrainReceiptLookup)
    }
}
