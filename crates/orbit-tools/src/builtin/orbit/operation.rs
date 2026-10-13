//! MCP adapter over the operations-as-data kernel.
//!
//! An operation is declared once in `orbit-common` and each surface derives
//! its wiring. This module turns every [`OperationSpec`] of a noun into one
//! registered [`OperationTool`]: the schema MCP advertises, the workspace scope
//! it is registered at, and its execution kind all come from the spec, so no
//! verb needs a hand-written [`Tool`] implementation.
//!
//! Every call is checked against its spec here, before dispatch: this is the
//! one entry every surface reaches (MCP, `orbit tool run`, the CLI, the
//! dashboard and the deterministic dispatcher), so they all share one
//! validation and the handler receives canonical input.
//!
//! A migrated noun supplies only its registry and a
//! `From<Verb> for OrbitBuiltinAction` impl in [`crate::host`].

use orbit_common::OrbitError;
use orbit_common::governance::operation::{Effect, NounSpec, OperationSpec, OperationVerb};
use orbit_types::tool::{ToolParam, ToolSchema};
use serde_json::Value;

use crate::{OrbitBuiltinAction, Tool, ToolContext, ToolExecutionKind, ToolRegistry};

/// One registry operation, exposed as a tool.
///
/// The spec supplies the schema, the exposure policy and the execution kind;
/// the verb it carries is what the runtime host dispatches on.
pub struct OperationTool<V: 'static>(pub &'static OperationSpec<V>);

impl<V> Tool for OperationTool<V>
where
    V: OperationVerb,
    OrbitBuiltinAction: From<V>,
{
    fn schema(&self) -> ToolSchema {
        operation_tool_schema(self.0)
    }

    fn execution_kind(&self) -> ToolExecutionKind {
        match self.0.effect {
            Effect::Read => ToolExecutionKind::ReadOnly,
            Effect::Write => ToolExecutionKind::Mutating,
        }
    }

    fn execute(&self, ctx: &ToolContext, input: Value) -> Result<Value, OrbitError> {
        let input = self.0.parse_input(input)?;
        super::report_unknown_fields(ctx, self.0.tool_name, &input);
        super::execute_host_action(ctx, input.into_value(), self.0.verb.into())
    }
}

/// Register every operation a noun declares, under the exposure each declares.
pub(super) fn register_noun<V>(registry: &mut ToolRegistry, noun: &'static NounSpec<V>)
where
    V: OperationVerb,
    OrbitBuiltinAction: From<V>,
{
    for spec in noun.operations {
        let tool = OperationTool(spec);
        match spec.mcp_scope {
            Some(scope) => registry.register_mcp(tool, scope),
            None => registry.register_inactive(tool),
        }
    }
}

/// Derive the MCP tool schema an operation advertises.
///
/// Parameter order follows the spec's declaration order, which is contract: it
/// is what the shipped `mcp_tools_list` snapshot records.
fn operation_tool_schema<V: 'static>(spec: &OperationSpec<V>) -> ToolSchema {
    ToolSchema {
        name: spec.tool_name.to_string(),
        description: spec.tool_description.to_string(),
        parameters: spec
            .mcp_params()
            .map(|(param, description)| ToolParam {
                name: param.name.to_string(),
                description: description.resolve(),
                param_type: param.param_type.as_tool_param_type().to_string(),
                required: param.required,
            })
            .collect(),
        builtin: true,
    }
}
