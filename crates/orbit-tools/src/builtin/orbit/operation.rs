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
//! A migrated noun supplies its registry, a `From<Verb> for OrbitBuiltinAction`
//! impl in [`crate::host`], and its [`GuardedModes`] table.

use orbit_common::OrbitError;
use orbit_common::governance::friction::FrictionVerb;
use orbit_common::governance::operation::{
    Effect, NounSpec, OperationInput, OperationSpec, OperationVerb, ToolOnlyRegistration,
};
use orbit_types::tool::{ToolParam, ToolSchema};
use serde_json::Value;

use crate::{OrbitBuiltinAction, Tool, ToolContext, ToolExecutionKind, ToolRegistry};

/// A noun's MCP surface table: the guarded modes of its verbs.
///
/// A guarded mode is an optional variant of a verb, selected by an input
/// field, that another host action serves under stricter checks (an
/// acknowledged unconditional mint, a compare-and-set toggle). Each noun
/// implements this as one exhaustive `match` on its verb.
pub(super) trait GuardedModes: OperationVerb {
    /// Serve a call that selects a guarded mode of this verb; `None` hands it
    /// to the verb's own handler.
    fn guarded_mode(
        self,
        ctx: &ToolContext,
        input: &OperationInput,
    ) -> Option<Result<Value, OrbitError>>;
}

impl GuardedModes for FrictionVerb {
    fn guarded_mode(
        self,
        _ctx: &ToolContext,
        _input: &OperationInput,
    ) -> Option<Result<Value, OrbitError>> {
        match self {
            FrictionVerb::Add
            | FrictionVerb::List
            | FrictionVerb::Show
            | FrictionVerb::Stats
            | FrictionVerb::Tags
            | FrictionVerb::Update
            | FrictionVerb::Resolve
            | FrictionVerb::Rehome => None,
        }
    }
}

/// One registry operation, exposed as a tool.
///
/// The spec supplies the schema, the exposure policy and the execution kind;
/// the verb it carries is what the runtime host dispatches on.
pub struct OperationTool<V: 'static>(pub &'static OperationSpec<V>);

impl<V> Tool for OperationTool<V>
where
    V: GuardedModes,
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

    fn execute(&self, ctx: &ToolContext, mut input: Value) -> Result<Value, OrbitError> {
        if let Some(host) = ctx.orbit_host.as_deref() {
            host.prepare_operation_input(self.0.verb.into(), &mut input)?;
        }
        let input = self.0.parse_input(input)?;
        if let Some(served) = self.0.verb.guarded_mode(ctx, &input) {
            return served;
        }
        super::report_unknown_fields(ctx, self.0.tool_name, &input);
        super::execute_host_action(ctx, input.into_value(), self.0.verb.into())
    }
}

/// Register every operation a noun declares, under the exposure each declares.
pub(super) fn register_noun<V>(registry: &mut ToolRegistry, noun: &'static NounSpec<V>)
where
    V: GuardedModes,
    OrbitBuiltinAction: From<V>,
{
    for spec in noun.operations {
        let tool = OperationTool(spec);
        match spec.mcp_scope {
            Some(scope) => registry.register_mcp(tool, scope),
            None => match noun.tool_only_registration {
                ToolOnlyRegistration::Active => registry.register(tool),
                ToolOnlyRegistration::Inactive => registry.register_inactive(tool),
            },
        }
    }
}

/// Derive the MCP tool schema an operation advertises.
///
/// Parameter order follows the spec's declaration order, which is contract: it
/// is what the shipped `mcp_tools_list` snapshot records.
pub(super) fn operation_tool_schema<V: 'static>(spec: &OperationSpec<V>) -> ToolSchema {
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
