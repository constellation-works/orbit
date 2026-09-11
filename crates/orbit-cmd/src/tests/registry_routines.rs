use chrono::Utc;
use orbit_core::application::routines::RoutineRegistryView;
use orbit_store::maintenance::task_registry::{WorkspaceConfig, write_workspace_config};
use orbit_types::workspace::{Workspace, WorkspaceCheckout, WorkspaceRegistry, WorkspaceStatus};

use orbit_registry::host_identity::load_host_identity;
use orbit_registry::workspace_registry;

use crate::registry_routines::{discover_registered_workspaces, load_routine_placement_at};

fn write_identity(root: &std::path::Path) {
    std::fs::create_dir_all(root).expect("global root");
    std::fs::write(
        root.join("host.toml"),
        "schema_version = 2\nmachine_id = \"hm_local\"\nhost_id = \"local\"\ntask_prefix = \"ORB\"\n",
    )
    .expect("host identity");
}

#[test]
fn routine_placement_uses_local_owner_names_without_fleet_state() {
    let root = tempfile::tempdir().expect("root");
    write_identity(root.path());
    let now = Utc::now();
    let registry = WorkspaceRegistry {
        owner_host_ids: [
            ("hm_local".to_string(), "local".to_string()),
            ("hm_remote".to_string(), "remote".to_string()),
        ]
        .into_iter()
        .collect(),
        workspaces: vec![
            Workspace {
                id: "ws_local".to_string(),
                name: "local-workspace".to_string(),
                owner_machine_id: Some("hm_local".to_string()),
                git_remote: None,
                ship_mode: None,
                base_branch: "main".to_string(),
                status: WorkspaceStatus::Active,
                created_at: now,
                updated_at: now,
            },
            Workspace {
                id: "ws_remote".to_string(),
                name: "remote-workspace".to_string(),
                owner_machine_id: Some("hm_remote".to_string()),
                git_remote: None,
                ship_mode: None,
                base_branch: "main".to_string(),
                status: WorkspaceStatus::Active,
                created_at: now,
                updated_at: now,
            },
        ],
        ..WorkspaceRegistry::default()
    };
    workspace_registry::save_registry_to(
        &registry,
        &workspace_registry::registry_path_for(root.path()),
    )
    .expect("local registry");
    // A malformed dormant cache is a canary: the v1 placement path must not
    // even attempt to parse it.
    std::fs::write(root.path().join("registry-cache.json"), "not-json").expect("cache canary");
    let identity = load_host_identity(root.path()).expect("identity");

    let placement = load_routine_placement_at(root.path(), &identity).expect("placement");
    assert_eq!(placement.local_host.host_id, "local");
    assert_eq!(
        placement.registry,
        RoutineRegistryView {
            owner_host_ids: ["remote".to_string()].into_iter().collect()
        }
    );
}

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
    assert_eq!(binding.workspace_id, "ws_runtime");
    assert_eq!(binding.ship_mode.as_input_value(), "pr");
}
