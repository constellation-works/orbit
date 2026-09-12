use chrono::Utc;
use orbit_store::maintenance::task_registry::{WorkspaceConfig, write_workspace_config};
use orbit_types::workspace::{
    Workspace, WorkspaceCheckout, WorkspaceCheckoutRole, WorkspaceRegistry, WorkspaceStatus,
};

use orbit_registry::workspace_registry;

use crate::registry_routines::discover_registered_workspaces;

#[test]
fn workspace_discovery_builds_bound_runtimes() {
    let root = tempfile::tempdir().expect("root");
    let global = root.path().join("global");
    let repo = root.path().join("repo");
    let orbit_dir = repo.join(".orbit");
    std::fs::create_dir_all(global.join("state")).expect("global");
    std::fs::create_dir_all(&orbit_dir).expect("orbit");
    std::fs::write(
        global.join("host.toml"),
        "schema_version = 2\nmachine_id = \"hm_local\"\nhost_id = \"local\"\ntask_prefix = \"ORB\"\n",
    )
    .expect("host identity");
    write_workspace_config(
        &orbit_dir,
        &WorkspaceConfig {
            schema_version: 1,
            workspace_id: "ws_runtime".to_string(),
        },
    )
    .expect("workspace config");
    let workspace = Workspace {
        id: "logical-abc123".to_string(),
        name: "orbit".to_string(),
        owner_machine_id: Some("hm_local".to_string()),
        git_remote: None,
        ship_mode: Some("pr".to_string()),
        base_branch: "agent-main".to_string(),
        status: WorkspaceStatus::Active,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    let registry = WorkspaceRegistry {
        workspaces: vec![workspace.clone()],
        checkouts: vec![WorkspaceCheckout::owner(
            workspace.id.clone(),
            repo,
            orbit_dir,
        )],
        ..WorkspaceRegistry::default()
    };
    workspace_registry::save_registry_to(
        &registry,
        &workspace_registry::registry_path_for(&global),
    )
    .expect("registry");

    let discovered = discover_registered_workspaces(&global, None).expect("discovery");
    assert!(discovered.errors.is_empty());
    assert_eq!(discovered.entries.len(), 1);
    let binding = discovered.entries[0]
        .1
        .workspace_runtime_binding()
        .expect("binding");
    assert_eq!(binding.task_partition_id, "ws_runtime");
    assert_eq!(binding.ship_mode.as_input_value(), "pr");
}

/// Registration is the automation opt-in [ORB-12236], but a replica cannot
/// write the owner's coordination store, so the clock never evaluates its
/// definitions.
#[test]
fn workspace_discovery_skips_replica_checkouts() {
    let root = tempfile::tempdir().expect("root");
    let global = root.path().join("global");
    std::fs::create_dir_all(global.join("state")).expect("global");
    std::fs::write(
        global.join("host.toml"),
        "schema_version = 2\nmachine_id = \"hm_local\"\nhost_id = \"local\"\ntask_prefix = \"ORB\"\n",
    )
    .expect("host identity");

    let mut workspaces = Vec::new();
    let mut checkouts = Vec::new();
    for (name, role) in [
        ("owned", WorkspaceCheckoutRole::Owner),
        ("replicated", WorkspaceCheckoutRole::Replica),
    ] {
        let repo = root.path().join(name);
        let orbit_dir = repo.join(".orbit");
        std::fs::create_dir_all(&orbit_dir).expect("orbit dir");
        write_workspace_config(
            &orbit_dir,
            &WorkspaceConfig {
                schema_version: 1,
                workspace_id: format!("ws_{name}"),
            },
        )
        .expect("workspace config");
        let owner_machine_id = match role {
            WorkspaceCheckoutRole::Owner => "hm_local",
            WorkspaceCheckoutRole::Replica => "hm_remote",
        };
        let workspace = Workspace {
            id: format!("logical-{name}"),
            name: name.to_string(),
            owner_machine_id: Some(owner_machine_id.to_string()),
            git_remote: None,
            ship_mode: None,
            base_branch: "agent-main".to_string(),
            status: WorkspaceStatus::Active,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let mut checkout = WorkspaceCheckout::owner(workspace.id.clone(), repo, orbit_dir);
        if role == WorkspaceCheckoutRole::Replica {
            checkout.role = Some(WorkspaceCheckoutRole::Replica);
            checkout.owner_machine_id = Some(owner_machine_id.to_string());
        }
        workspaces.push(workspace);
        checkouts.push(checkout);
    }
    let registry = WorkspaceRegistry {
        workspaces,
        checkouts,
        ..WorkspaceRegistry::default()
    };
    workspace_registry::save_registry_to(
        &registry,
        &workspace_registry::registry_path_for(&global),
    )
    .expect("registry");

    let discovered = discover_registered_workspaces(&global, None).expect("discovery");
    assert!(discovered.errors.is_empty());
    assert_eq!(
        discovered
            .entries
            .iter()
            .map(|(workspace, _)| workspace.name.as_str())
            .collect::<Vec<_>>(),
        vec!["owned"]
    );
}
