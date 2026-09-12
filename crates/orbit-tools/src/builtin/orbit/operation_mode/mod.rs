//! Operation-mode tools, derived from the operation-mode registry
//! [ORB-11332].
//!
//! Every `orbit.operation.*` verb is declared once in
//! `orbit_common::governance::operation_mode`; this module turns each spec into
//! a registered tool. `mcp_scope: None` keeps them off MCP. Authorization is
//! not decided here: the governed verbs (`enable`, `stop`, `revoke`) are
//! refused at the runtime chokepoint for a caller without the operator
//! capability.

use orbit_common::OrbitError;
use orbit_common::governance::operation_mode::{OPERATION_MODE_OPERATIONS, OperationModeOperation};
use orbit_types::tool::ToolSchema;
use serde_json::Value;

use super::operation::{operation_tool_schema, register_operation};
use crate::{OrbitBuiltinAction, Tool, ToolContext, ToolRegistry};

/// One operation-mode verb, registered from its spec.
pub struct OperationModeTool(pub &'static OperationModeOperation);

impl Tool for OperationModeTool {
    fn schema(&self) -> ToolSchema {
        operation_tool_schema(self.0)
    }

    fn execute(&self, ctx: &ToolContext, input: Value) -> Result<Value, OrbitError> {
        super::execute_host_action(ctx, input, OrbitBuiltinAction::OperationMode(self.0.verb))
    }
}

/// Register every operation-mode verb the registry declares.
pub(super) fn register(registry: &mut ToolRegistry) {
    for spec in OPERATION_MODE_OPERATIONS {
        register_operation(registry, spec, OperationModeTool(spec));
    }
}

#[cfg(test)]
mod tests;
