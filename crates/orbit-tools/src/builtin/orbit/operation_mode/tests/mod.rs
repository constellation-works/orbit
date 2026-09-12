//! The operation-mode MCP surface is derived from the registry.

use orbit_common::governance::operation_mode::{OPERATION_MODE_OPERATIONS, OperationModeVerb};
use orbit_types::tool::McpToolScope;

use super::OperationModeTool;
use crate::{Tool, ToolRegistry};

#[test]
fn every_verb_is_registered_and_advertised_per_its_spec() {
    let mut registry = ToolRegistry::new();
    registry.register_builtins();
    let advertised = registry
        .mcp_tool_definitions()
        .expect("valid MCP definitions")
        .into_iter()
        .map(|definition| definition.schema.name)
        .collect::<Vec<_>>();
    for spec in OPERATION_MODE_OPERATIONS {
        assert!(
            registry.has(spec.tool_name),
            "{} is registered",
            spec.tool_name
        );
        assert_eq!(
            advertised.iter().any(|name| name == spec.tool_name),
            spec.mcp_scope == Some(McpToolScope::WorkspaceRequired),
            "{} advertisement follows its spec",
            spec.tool_name
        );
    }
}

#[test]
fn derived_schema_carries_the_declared_parameters_in_order() {
    let schema = OperationModeTool(OperationModeVerb::Enable.spec()).schema();
    assert_eq!(schema.name, "orbit.operation.enable");
    let names: Vec<&str> = schema
        .parameters
        .iter()
        .map(|param| param.name.as_str())
        .collect();
    assert_eq!(
        names,
        vec![
            "task_ids",
            "window",
            "rights",
            "preset",
            "completion",
            "leaf_ceiling",
            "recovery_episodes",
            "recovery_minutes",
            "review_policy",
            "claim_token",
            "model",
        ]
    );
    assert_eq!(schema.parameters[0].param_type, "string_list");
    assert!(
        schema.parameters[0].required
            && schema.parameters[1].required
            && schema.parameters[2].required
    );
    assert!(!schema.parameters[3].required);
}
