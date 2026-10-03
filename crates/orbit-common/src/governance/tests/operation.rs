//! Kernel-level tests for the operations-as-data primitives.

use super::super::operation::*;
use orbit_types::tool::McpToolScope;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DemoVerb {
    Ping,
}

const DEMO: OperationSpec<DemoVerb> = OperationSpec {
    verb: DemoVerb::Ping,
    name: "ping",
    tool_name: "demo.ping",
    tool_description: "Ping the demo noun",
    cli_about: "Ping the demo noun",
    params: &[
        ParamSpec {
            name: "id",
            param_type: ParamType::String,
            required: true,
            mcp_description: Some(Description::Static("demo ID")),
            cli: Some(CliBinding {
                kind: CliArgKind::Positional,
                help: Description::Static("Demo record id"),
            }),
        },
        ParamSpec {
            name: "labels",
            param_type: ParamType::StringList,
            required: false,
            mcp_description: Some(Description::Computed(|| "computed labels".to_string())),
            cli: Some(CliBinding {
                kind: CliArgKind::Flag {
                    long: "label",
                    delimiter: Some(','),
                },
                help: Description::Static("Demo label"),
            }),
        },
        ParamSpec {
            name: "cli_only",
            param_type: ParamType::String,
            required: false,
            mcp_description: None,
            cli: Some(CliBinding {
                kind: CliArgKind::Flag {
                    long: "cli-only",
                    delimiter: None,
                },
                help: Description::Static("CLI-only knob"),
            }),
        },
        ParamSpec {
            name: "mcp_only",
            param_type: ParamType::Integer,
            required: false,
            mcp_description: Some(Description::Static("MCP-only knob")),
            cli: None,
        },
    ],
    rejects_agent_field: false,
    mcp_scope: Some(McpToolScope::WorkspaceRequired),
    cli_json_flag: true,
    cli_render: CliRender::Record,
};

#[test]
fn param_type_tokens_are_the_tool_schema_vocabulary() {
    assert_eq!(ParamType::String.as_tool_param_type(), "string");
    assert_eq!(ParamType::StringList.as_tool_param_type(), "string_list");
    assert_eq!(ParamType::Integer.as_tool_param_type(), "integer");
    assert_eq!(ParamType::Boolean.as_tool_param_type(), "boolean");
}

#[test]
fn descriptions_resolve_static_and_computed_forms() {
    assert_eq!(Description::Static("fixed").resolve(), "fixed");
    assert_eq!(
        Description::Computed(|| "made up".to_string()).resolve(),
        "made up"
    );
}

#[test]
fn surface_projections_respect_per_param_opt_outs() {
    let mcp: Vec<&str> = DEMO.mcp_params().map(|(param, _)| param.name).collect();
    assert_eq!(mcp, vec!["id", "labels", "mcp_only"]);

    let cli: Vec<&str> = DEMO.cli_params().map(|(param, _)| param.name).collect();
    assert_eq!(cli, vec!["id", "labels", "cli_only"]);
}

#[test]
fn cli_value_names_match_clap_derive_conventions() {
    let params: Vec<String> = DEMO
        .params
        .iter()
        .map(|param| param.cli_value_name())
        .collect();
    assert_eq!(params, vec!["ID", "LABELS", "CLI_ONLY", "MCP_ONLY"]);
}

#[test]
fn cli_long_is_the_flag_spelling_not_the_wire_name() {
    let labels = DEMO
        .params
        .iter()
        .find(|param| param.name == "labels")
        .expect("labels param");
    assert_eq!(labels.cli_long(), Some("label"));

    let id = DEMO
        .params
        .iter()
        .find(|param| param.name == "id")
        .expect("id param");
    assert_eq!(id.cli_long(), None);
}

#[test]
fn cli_positional_finds_the_audit_target_param() {
    assert_eq!(DEMO.cli_positional().map(|param| param.name), Some("id"));
}

#[test]
fn find_by_name_matches_the_short_verb_name() {
    let registry = [DEMO];
    assert_eq!(
        find_by_name(&registry, "ping").map(|spec| spec.tool_name),
        Some("demo.ping")
    );
    assert!(find_by_name(&registry, "pong").is_none());
}
