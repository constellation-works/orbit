//! The friction MCP surface is derived from the operation registry and must
//! preserve the shipped schemas.
//
// Sibling layout under `friction/tests/` follows
// docs/design-patterns/test_layout.md.

use orbit_common::governance::friction::{FRICTION_OPERATIONS, FrictionVerb};
use orbit_types::tool::{McpToolScope, ToolSchema};

use super::super::FrictionOperationTool;
use crate::{Tool, ToolRegistry};

fn schema_for(verb: FrictionVerb) -> ToolSchema {
    FrictionOperationTool(verb.spec()).schema()
}

fn param_shape(schema: &ToolSchema) -> Vec<(&str, &str, bool)> {
    schema
        .parameters
        .iter()
        .map(|param| {
            (
                param.name.as_str(),
                param.param_type.as_str(),
                param.required,
            )
        })
        .collect()
}

/// The `add` schema. Parameter order is part of the contract: it drives the
/// `mcp_tools_list` snapshot. `title` was appended after `body` by [ORB-10590],
/// which gave friction authors a settable record handle; every other parameter
/// keeps the position it shipped with.
#[test]
fn add_schema_matches_the_shipped_contract() {
    let schema = schema_for(FrictionVerb::Add);

    assert_eq!(schema.name, "orbit.friction.add");
    assert!(schema.builtin);
    assert_eq!(
        param_shape(&schema),
        vec![
            ("body", "string", true),
            ("title", "string", false),
            ("tags", "string_list", false),
            ("during_task", "string", false),
            ("model", "string", true),
        ]
    );
}

#[test]
fn list_schema_matches_the_shipped_contract() {
    let schema = schema_for(FrictionVerb::List);

    assert_eq!(schema.name, "orbit.friction.list");
    assert_eq!(
        param_shape(&schema),
        vec![
            ("model", "string", false),
            ("status", "string", false),
            ("tag", "string", false),
            ("month", "string", false),
            ("q", "string", false),
            ("from", "string", false),
            ("to", "string", false),
            ("limit", "integer", false),
            ("offset", "integer", false),
            ("response_mode", "string", false),
        ]
    );
}

/// `show` and `resolve` share the required ID parameter shape.
#[test]
fn id_only_schemas_require_an_identifier() {
    for verb in [FrictionVerb::Show, FrictionVerb::Resolve] {
        let schema = schema_for(verb);
        assert_eq!(param_shape(&schema), vec![("id", "string", true)]);
    }
}

#[test]
fn update_schema_exposes_expected_parameters() {
    let schema = schema_for(FrictionVerb::Update);
    assert_eq!(
        param_shape(&schema),
        vec![
            ("id", "string", true),
            ("status", "string", false),
            ("tags", "string_list", false),
            ("body", "string", false),
            ("rehome_to", "string", false),
            ("title", "string", false),
        ]
    );
}

#[test]
fn aggregate_verbs_take_no_parameters() {
    for verb in [FrictionVerb::Stats, FrictionVerb::Tags] {
        assert!(schema_for(verb).parameters.is_empty());
    }
}

#[test]
fn registration_reproduces_the_shipped_mcp_surface() {
    let mut registry = ToolRegistry::new();
    super::super::register(&mut registry);

    let definitions = registry
        .mcp_tool_definitions()
        .expect("friction MCP definitions are valid");
    let mut advertised: Vec<&str> = definitions
        .iter()
        .map(|definition| definition.schema.name.as_str())
        .collect();
    advertised.sort_unstable();
    assert_eq!(
        advertised,
        vec![
            "orbit.friction.add",
            "orbit.friction.list",
            "orbit.friction.rehome",
            "orbit.friction.update",
        ],
        "show, tags, stats, and resolve stay off the MCP surface"
    );
    for definition in &definitions {
        assert_eq!(definition.scope, McpToolScope::WorkspaceRequired);
    }

    // Every verb stays reachable through the CLI / dashboard `run_tool` path,
    // advertised or not.
    for spec in FRICTION_OPERATIONS {
        assert!(
            registry.has(spec.tool_name),
            "{} must be registered",
            spec.tool_name
        );
    }
}
