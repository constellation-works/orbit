use std::path::PathBuf;

use chrono::Utc;
use orbit_common::protocol::tool_schema::tool_input_schema;
use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::tool::McpToolScope;
use orbit_types::workspace::{
    Workspace, WorkspaceCheckout, WorkspaceCheckoutRole, WorkspaceRegistry, WorkspaceStatus,
};
use serde_json::json;

use super::super::discovery::{
    discovery_tool_definitions, execute_discovery_tool, execute_federated_workspace_discovery,
};
use super::super::surface::canonical_mcp_tool_definitions;

#[test]
fn mcp_owns_the_exact_global_discovery_definitions() {
    let definitions = discovery_tool_definitions().expect("discovery definitions");
    assert_eq!(definitions.len(), 2);
    assert_eq!(
        definitions
            .iter()
            .map(|definition| definition.schema.name.as_str())
            .collect::<Vec<_>>(),
        ["orbit.workspace.list", "orbit.crew.list"]
    );
    assert_eq!(
        definitions[0].schema.description,
        "List active workspaces with a checkout registered on this machine."
    );
    let workspace = &definitions[0];
    assert!(workspace.schema.builtin);
    assert!(workspace.schema.parameters.is_empty());
    assert_eq!(workspace.scope, McpToolScope::Global);
    assert_eq!(
        tool_input_schema(&workspace.schema),
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": true,
        })
        .as_object()
        .expect("object schema")
        .clone()
    );

    let crew = &definitions[1];
    assert_eq!(crew.schema.name, "orbit.crew.list");
    assert_eq!(
        crew.schema.description,
        "List the effective configured crews for a selected workspace on this machine."
    );
    assert!(crew.schema.builtin);
    assert_eq!(crew.scope, McpToolScope::WorkspaceRequired);
    assert_eq!(crew.schema.parameters.len(), 1);
    assert_eq!(crew.schema.parameters[0].name, "workspace");

    let canonical = canonical_mcp_tool_definitions().expect("canonical definitions");
    assert_eq!(canonical.len(), 23, "the frozen production surface changed");
    assert_eq!(
        canonical
            .iter()
            .filter(|definition| definition.scope == McpToolScope::Global)
            .map(|definition| definition.schema.name.as_str())
            .collect::<Vec<_>>(),
        ["orbit.workspace.list"]
    );
}

#[test]
fn discovery_projects_active_workspaces_with_a_local_checkout() {
    let now = Utc::now();
    let workspace = |id: &str, owner: &str, status| Workspace {
        id: id.to_string(),
        name: id.to_string(),
        owner_machine_id: Some(owner.to_string()),
        git_remote: None,
        ship_mode: None,
        base_branch: "main".to_string(),
        status,
        created_at: now,
        updated_at: now,
    };
    let checkout = |id: &str, role, owner_machine_id| WorkspaceCheckout {
        workspace_id: id.to_string(),
        repo_root: PathBuf::from(format!("/tmp/{id}")),
        orbit_dir: PathBuf::from(format!("/tmp/{id}/.orbit")),
        role: Some(role),
        owner_machine_id,
        path_overrides: Vec::new(),
    };
    let registry = WorkspaceRegistry {
        workspaces: vec![
            workspace("ws_local", "hm_local", WorkspaceStatus::Active),
            workspace("ws_replica", "hm_remote", WorkspaceStatus::Active),
            workspace("ws_checkoutless", "hm_local", WorkspaceStatus::Active),
            workspace("ws_invalid", "hm_local", WorkspaceStatus::Invalid),
        ],
        checkouts: vec![
            checkout("ws_local", WorkspaceCheckoutRole::Owner, None),
            checkout(
                "ws_replica",
                WorkspaceCheckoutRole::Replica,
                Some("hm_remote".to_string()),
            ),
            checkout("ws_invalid", WorkspaceCheckoutRole::Owner, None),
        ],
        ..WorkspaceRegistry::default()
    };

    let listed = execute_discovery_tool("orbit.workspace.list", &registry, "hm_local")
        .expect("workspace projection");
    assert_eq!(listed["machine_id"], "hm_local");
    assert_eq!(
        listed["workspaces"]
            .as_array()
            .expect("rows")
            .iter()
            .map(|workspace| workspace["id"].as_str().expect("workspace id"))
            .collect::<Vec<_>>(),
        ["ws_local", "ws_replica"]
    );

    let federated = execute_federated_workspace_discovery(&registry, "hm_local");
    assert_eq!(federated["machine_id"], "hm_local");
    assert_eq!(
        federated["workspaces"]
            .as_array()
            .expect("federated rows")
            .iter()
            .map(|workspace| workspace["id"].as_str().expect("workspace id"))
            .collect::<Vec<_>>(),
        ["ws_local", "ws_replica", "ws_invalid"],
        "the private destination path retains Invalid local checkouts",
    );
    assert!(matches!(
        execute_discovery_tool("orbit.host.future", &registry, "hm_local"),
        Err(OrbitError::NotFound {
            kind: NotFoundKind::Tool,
            ..
        })
    ));
}
