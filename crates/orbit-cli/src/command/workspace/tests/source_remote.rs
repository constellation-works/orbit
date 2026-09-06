use std::path::Path;

use chrono::Utc;
use orbit_core::OrbitError;
use orbit_registry::workspace_registry;
use orbit_types::workspace::{
    Workspace, WorkspaceCheckout, WorkspaceCheckoutRole, WorkspaceRegistry, WorkspaceStatus,
};
use tempfile::tempdir;

use super::super::source_remote::rebind_at_registry_path;

fn registry(root: &Path) -> WorkspaceRegistry {
    WorkspaceRegistry {
        workspaces: vec![Workspace {
            id: "ws_orbit".to_string(),
            name: "orbit".to_string(),
            owner_machine_id: Some("hm_owner".to_string()),
            git_remote: Some("git@github.com:example/orbit.git".to_string()),
            ship_mode: Some("pr".to_string()),
            base_branch: "agent-main".to_string(),
            status: WorkspaceStatus::Active,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }],
        checkouts: vec![WorkspaceCheckout {
            workspace_id: "ws_orbit".to_string(),
            repo_root: root.join("repo"),
            orbit_dir: root.join("repo/.orbit"),
            role: Some(WorkspaceCheckoutRole::Owner),
            owner_machine_id: None,
            path_overrides: vec![root.join("linked")],
        }],
        ..Default::default()
    }
}

#[test]
fn injected_persistence_failure_leaves_registry_file_unchanged() {
    let root = tempdir().expect("tempdir");
    std::fs::write(
        root.path().join("host.toml"),
        "schema_version = 2\nmachine_id = \"hm_owner\"\nhost_id = \"owner\"\ntask_prefix = \"ORB\"\n",
    )
    .expect("host identity");
    let path = root.path().join("workspaces.json");
    workspace_registry::save_registry_to(&registry(root.path()), &path)
        .expect("write fixture registry");
    let before = std::fs::read(&path).expect("read fixture");

    let error = rebind_at_registry_path(
        &path,
        "ws_orbit",
        "ssh://github.com/example/orbit-renamed.git",
        "hm_owner",
        false,
        |_, _| Err(OrbitError::Io("injected write failure".to_string())),
    )
    .expect_err("write failure must surface");

    assert!(error.to_string().contains("injected write failure"));
    assert_eq!(
        std::fs::read(&path).expect("read preserved registry"),
        before
    );
}
