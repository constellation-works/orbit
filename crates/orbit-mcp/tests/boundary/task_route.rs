//! The id route each builtin declares at its definition site stays in step
//! with the canonical surface: a tool that addresses one id with an
//! optional workspace must declare a route, and a declared route must
//! match that parameter shape.

use orbit_tools::{ToolRegistry, builtin_mcp_rules};
use orbit_types::tool::{McpIdRoute, McpToolScope, ToolSchema, tool_family};

/// Every builtin, including the CLI-only tools `orbit tool run` reaches. A
/// family stands for its actions' operations, which a call routes by.
fn builtins() -> (ToolRegistry, Vec<ToolSchema>) {
    let mut registry = ToolRegistry::new();
    registry.register_builtins();
    let schemas = registry
        .all_schemas()
        .into_iter()
        .flat_map(|schema| match tool_family(&schema.name) {
            Some(family) => family
                .actions
                .iter()
                .filter_map(|action| registry.get_schema(action.operation))
                .collect(),
            None => vec![schema],
        })
        .collect();
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
        .filter(|name| builtin_mcp_rules(name).id_route == McpIdRoute::Unspecified)
        .collect::<Vec<_>>();
    assert!(
        unclassified.is_empty(),
        "tools with a required `id` and an optional workspace must declare a McpIdRoute at \
         their definition site (task-id-routed, or workspace-local with a reason), so a task id \
         never silently stops routing to the host its prefix names: {unclassified:?}"
    );
}

#[test]
fn every_declared_route_matches_one_id_with_optional_workspace() {
    let (registry, schemas) = builtins();
    for schema in &schemas {
        let route = builtin_mcp_rules(&schema.name).id_route;
        if route == McpIdRoute::Unspecified {
            continue;
        }
        assert!(
            addresses_one_id_with_optional_workspace(&registry, schema),
            "{} is classified for id routing but does not take a required `id` with an \
             optional workspace; drop the declared route from its definition",
            schema.name
        );
        if let McpIdRoute::WorkspaceLocal(reason) = &route {
            assert!(
                !reason.trim().is_empty(),
                "{} needs a workspace-local reason",
                schema.name
            );
        }
    }
}

#[test]
fn id_only_task_target_accepts_task_id_alias_when_id_absent() {
    use orbit_mcp::federated::id_only_task_target;
    use serde_json::json;

    assert_eq!(
        id_only_task_target("orbit.task.show", &json!({"task_id": " ORB-1 "})),
        Some("ORB-1")
    );
    assert_eq!(
        id_only_task_target("orbit.task.update", &json!({"task_id": "ORB-2"})),
        Some("ORB-2")
    );
    assert_eq!(
        id_only_task_target(
            "orbit.task.artifact",
            &json!({"action": "get", "task_id": "ORB-3", "path": "a.txt"})
        ),
        Some("ORB-3")
    );
    assert_eq!(
        id_only_task_target(
            "orbit.task.show",
            &json!({"id": "ORB-1", "task_id": "ORB-2"})
        ),
        None
    );
    assert_eq!(
        id_only_task_target("orbit.task.eligible", &json!({"task_id": "ORB-1"})),
        None
    );
}
