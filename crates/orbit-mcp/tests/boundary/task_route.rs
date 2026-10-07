//! The id-routed task tool list stays in step with the canonical surface.

use orbit_mcp::federated::{ID_ROUTED_TASK_TOOLS, NOT_ID_ROUTED_TOOLS};
use orbit_tools::ToolRegistry;
use orbit_types::tool::{McpToolScope, ToolSchema};

/// Every builtin, including the CLI-only tools `orbit tool run` reaches.
fn builtins() -> (ToolRegistry, Vec<ToolSchema>) {
    let mut registry = ToolRegistry::new();
    registry.register_builtins();
    let schemas = registry.all_schemas();
    (registry, schemas)
}

/// A tool whose target is a required `id` and whose workspace is optional:
/// the tool declares it without requiring it, or leaves it to the selector
/// the MCP host adds to a workspace-scoped tool or the CLI's `--workspace`.
/// A global MCP tool opens no workspace and is out of scope.
fn addresses_one_id_with_optional_workspace(registry: &ToolRegistry, schema: &ToolSchema) -> bool {
    let parameters = &schema.parameters;
    let id_required = parameters
        .iter()
        .any(|parameter| parameter.name == "id" && parameter.required);
    let declared_workspace = parameters
        .iter()
        .find(|parameter| parameter.name == "workspace");
    let workspace_optional = match declared_workspace {
        Some(parameter) => !parameter.required,
        None => registry.mcp_scope(&schema.name) != Some(McpToolScope::Global),
    };
    id_required && workspace_optional
}

#[test]
fn every_tool_addressing_one_id_is_routed_or_excluded_with_a_reason() {
    let (registry, schemas) = builtins();
    let unclassified = schemas
        .iter()
        .filter(|schema| addresses_one_id_with_optional_workspace(&registry, schema))
        .map(|schema| schema.name.as_str())
        .filter(|name| {
            !ID_ROUTED_TASK_TOOLS.contains(name)
                && !NOT_ID_ROUTED_TOOLS
                    .iter()
                    .any(|(excluded, _)| excluded == name)
        })
        .collect::<Vec<_>>();
    assert!(
        unclassified.is_empty(),
        "tools with a required `id` and an optional workspace must be added to \
         ID_ROUTED_TASK_TOOLS or excluded in NOT_ID_ROUTED_TOOLS with a reason, so a task id \
         never silently stops routing to the host its prefix names: {unclassified:?}"
    );
}

#[test]
fn every_listed_tool_exists_and_addresses_one_id_with_optional_workspace() {
    let (registry, schemas) = builtins();
    let listed = ID_ROUTED_TASK_TOOLS
        .iter()
        .chain(NOT_ID_ROUTED_TOOLS.iter().map(|(name, _)| name));
    for name in listed {
        let schema = schemas
            .iter()
            .find(|schema| schema.name == *name)
            .unwrap_or_else(|| panic!("{name} is classified but is not a builtin tool"));
        assert!(
            addresses_one_id_with_optional_workspace(&registry, schema),
            "{name} is classified for id routing but does not take a required `id` with an \
             optional workspace; drop it from the list"
        );
    }
    for (name, reason) in NOT_ID_ROUTED_TOOLS {
        assert!(
            !reason.trim().is_empty(),
            "{name} needs an exclusion reason"
        );
    }
}
