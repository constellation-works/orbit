//! Friction MCP tools, derived from the friction operation registry.
//!
//! ADR-0209 bearing 1 pilot [ORB-10358]. Every friction verb used to carry a
//! hand-written `Tool` impl restating its name, description, and parameters —
//! the same facts the CLI and dashboard restated again. All of that now lives
//! once in `orbit_common::governance::friction::operations`; this module is the adapter
//! that turns each spec into a registered tool.
//!
//! Adding a friction verb requires no edit here.

use orbit_common::OrbitError;
use orbit_common::governance::friction::{FRICTION_OPERATIONS, FrictionOperation, FrictionVerb};
use orbit_types::tool::ToolSchema;
use serde_json::Value;

use super::operation::{operation_tool_schema, register_operation};
use crate::{OrbitBuiltinAction, Tool, ToolContext, ToolExecutionKind, ToolRegistry};

/// One friction verb, exposed as an MCP tool.
///
/// The spec supplies the schema and the exposure policy; the verb it carries is
/// what the runtime host dispatches on.
pub struct FrictionOperationTool(pub &'static FrictionOperation);

impl Tool for FrictionOperationTool {
    fn schema(&self) -> ToolSchema {
        operation_tool_schema(self.0)
    }

    fn execution_kind(&self) -> ToolExecutionKind {
        match self.0.verb {
            FrictionVerb::List | FrictionVerb::Show | FrictionVerb::Stats | FrictionVerb::Tags => {
                ToolExecutionKind::ReadOnly
            }
            _ => ToolExecutionKind::Mutating,
        }
    }

    fn execute(&self, ctx: &ToolContext, input: Value) -> Result<Value, OrbitError> {
        if self.0.rejects_agent_field {
            super::reject_agent_field(&input, self.0.tool_name)?;
        }
        super::execute_host_action(ctx, input, OrbitBuiltinAction::Friction(self.0.verb))
    }
}

/// Register every friction verb the registry declares.
pub(super) fn register(registry: &mut ToolRegistry) {
    for spec in FRICTION_OPERATIONS {
        register_operation(registry, spec, FrictionOperationTool(spec));
    }
}
