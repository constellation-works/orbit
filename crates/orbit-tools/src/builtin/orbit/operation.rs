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
//! impl in [`crate::host`], and its [`GuardedModes`] table: guarded modes,
//! input the session supplies, and schema extensions.

use orbit_common::OrbitError;
use orbit_common::governance::friction::FrictionVerb;
use orbit_common::governance::operation::{
    Effect, NounSpec, OperationInput, OperationSpec, OperationVerb, ToolOnlyRegistration,
};
use orbit_common::protocol::tool_schema::tool_input_schema;
use orbit_types::tool::{ToolParam, ToolSchema};
use serde_json::Value;

use crate::{OrbitBuiltinAction, Tool, ToolContext, ToolExecutionKind, ToolRegistry};

/// A noun's MCP surface table: what the tool entry does for a verb beyond
/// what its spec declares.
///
/// A guarded mode is an optional variant of a verb, selected by an input
/// field, that another host action serves under stricter checks (an
/// acknowledged unconditional mint, a compare-and-set toggle). Each noun
/// implements every method as one exhaustive `match` on its verb.
pub(super) trait GuardedModes: OperationVerb {
    /// Serve or refuse a call before its spec checks the input: a guarded
    /// mode that owns its whole input contract, or a refusal that must
    /// precede every field check. `None` hands the call on.
    fn preempt(self, ctx: &ToolContext, input: &Value) -> Option<Result<Value, OrbitError>>;

    /// Serve a call that selects a guarded mode of this verb; `None` hands it
    /// to the verb's own handler.
    fn guarded_mode(
        self,
        ctx: &ToolContext,
        input: &OperationInput,
    ) -> Option<Result<Value, OrbitError>>;

    /// Complete canonical input bound for the verb's own handler with what
    /// the calling session supplies, such as its workspace, or refuse it.
    fn shape_input(self, ctx: &ToolContext, input: &mut Value) -> Result<(), OrbitError>;

    /// JSON Schema the flat parameter list cannot express, such as a guarded
    /// mode's conditionals, applied to the derived input schema. Only MCP
    /// reads it.
    fn schema_extension(self) -> Option<fn(&mut Value)>;
}

impl GuardedModes for FrictionVerb {
    fn preempt(self, _ctx: &ToolContext, _input: &Value) -> Option<Result<Value, OrbitError>> {
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

    fn shape_input(self, _ctx: &ToolContext, _input: &mut Value) -> Result<(), OrbitError> {
        match self {
            FrictionVerb::Add
            | FrictionVerb::List
            | FrictionVerb::Show
            | FrictionVerb::Stats
            | FrictionVerb::Tags
            | FrictionVerb::Update
            | FrictionVerb::Resolve
            | FrictionVerb::Rehome => Ok(()),
        }
    }

    fn schema_extension(self) -> Option<fn(&mut Value)> {
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

    fn input_schema(&self) -> Option<Value> {
        let extend = self.0.verb.schema_extension()?;
        let mut schema = Value::Object(tool_input_schema(&self.schema()));
        extend(&mut schema);
        Some(schema)
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
        if let Some(served) = self.0.verb.preempt(ctx, &input) {
            return served;
        }
        let input = self.0.parse_input(input)?;
        if let Some(served) = self.0.verb.guarded_mode(ctx, &input) {
            return served;
        }
        super::report_unknown_fields(ctx, self.0.tool_name, &input);
        let mut input = input.into_value();
        self.0.verb.shape_input(ctx, &mut input)?;
        super::execute_host_action(ctx, input, self.0.verb.into())
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
