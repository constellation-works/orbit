//! The friction MCP surface is derived from the operation registry and must
//! preserve the shipped schemas.
//
// Sibling layout under `friction/tests/` follows
// docs/design-patterns/test_layout.md.

use orbit_common::governance::friction::{
    DEFAULT_FRICTION_TAGS, FRICTION_OPERATIONS, FRICTION_TITLE_MAX_CHARS, FrictionVerb,
    friction_tags_literal,
};
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

#[test]
fn tags_parameter_description_lists_default_taxonomy() {
    let schema = schema_for(FrictionVerb::Add);
    let tags_param = schema
        .parameters
        .iter()
        .find(|param| param.name == "tags")
        .expect("tags parameter");

    assert!(
        tags_param.description.contains(&friction_tags_literal()),
        "{}",
        tags_param.description
    );
    for (tag, _description) in DEFAULT_FRICTION_TAGS {
        assert!(
            tags_param.description.contains(tag),
            "tags description should include {tag}: {}",
            tags_param.description
        );
    }
}

/// The `add` schema. Parameter order is part of the contract: it drives the
/// `mcp_tools_list` snapshot. `title` was appended after `body` by [ORB-10590],
/// which gave friction authors a settable record handle; every other parameter
/// keeps the position and wording it shipped with.
#[test]
fn add_schema_matches_the_shipped_contract() {
    let schema = schema_for(FrictionVerb::Add);

    assert_eq!(schema.name, "orbit.friction.add");
    assert_eq!(schema.description, "Append a friction report");
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
    assert_eq!(
        schema.parameters[0].description,
        "Markdown body describing what happened and why it caused friction"
    );
    assert_eq!(
        schema.parameters[3].description,
        "Optional task ID being worked on when friction occurred"
    );
    assert_eq!(
        schema.parameters[4].description,
        "Required agent family for attribution (`codex`, `claude`, `gemini`, or `grok`)"
    );
}

/// Both write verbs describe the title budget from the one constant that
/// enforces it, so the schema cannot drift from the validator.
#[test]
fn title_parameter_descriptions_quote_the_enforced_budget() {
    for verb in [FrictionVerb::Add, FrictionVerb::Update] {
        let schema = schema_for(verb);
        let title = schema
            .parameters
            .iter()
            .find(|param| param.name == "title")
            .expect("title parameter");

        assert!(
            title
                .description
                .contains(&FRICTION_TITLE_MAX_CHARS.to_string()),
            "{}",
            title.description
        );
    }
}

#[test]
fn list_schema_matches_the_shipped_contract() {
    let schema = schema_for(FrictionVerb::List);

    assert_eq!(schema.name, "orbit.friction.list");
    assert!(schema.description.contains("JSON record array by default"));
    assert!(schema.description.contains("`{records, notes}`"));
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
    assert_eq!(
        schema.parameters[9].description,
        "Optional response shape: `with_notes` returns `{records, notes}`; omit for the legacy record array"
    );
}

/// `show` and `resolve` shared `orbit_id_params("friction")` before the
/// registry; the derived schema keeps that exact wording.
#[test]
fn id_only_schemas_keep_the_shared_id_parameter_wording() {
    for verb in [FrictionVerb::Show, FrictionVerb::Resolve] {
        let schema = schema_for(verb);
        assert_eq!(param_shape(&schema), vec![("id", "string", true)]);
        assert_eq!(schema.parameters[0].description, "friction ID");
    }
}

#[test]
fn update_schema_keeps_its_spelled_out_id_description() {
    let schema = schema_for(FrictionVerb::Update);
    let params: Vec<&str> = schema
        .parameters
        .iter()
        .map(|param| param.name.as_str())
        .collect();
    assert_eq!(params, vec!["id", "status", "tags", "body", "title"]);
    assert_eq!(
        schema.parameters[0].description,
        "Friction record ID, e.g. FYYYY-MM-NNN"
    );
}

#[test]
fn aggregate_verbs_take_no_parameters() {
    for verb in [FrictionVerb::Stats, FrictionVerb::Tags] {
        assert!(schema_for(verb).parameters.is_empty());
    }

    assert_eq!(
        schema_for(FrictionVerb::Stats).description,
        "Compute friction rates from the friction and task stores"
    );
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
