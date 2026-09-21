use chrono::{TimeZone, Utc};

use crate::workspace::{
    WORKSPACE_REGISTRY_SCHEMA_VERSION, Workspace, WorkspaceCheckout, WorkspaceCheckoutRole,
    WorkspaceRegistry, WorkspaceStatus,
};

#[test]
fn registry_serialization_keeps_paths_out_of_logical_workspaces() {
    let now = Utc
        .with_ymd_and_hms(2026, 7, 18, 1, 2, 3)
        .single()
        .expect("fixed timestamp");
    let registry = WorkspaceRegistry {
        workspaces: vec![Workspace {
            id: "ws_orbit".to_string(),
            name: "orbit".to_string(),
            owner_machine_id: Some("hm_owner".to_string()),
            git_remote: None,
            ship_mode: Some("pr".to_string()),
            base_branch: "agent-main".to_string(),
            status: WorkspaceStatus::Active,
            created_at: now,
            updated_at: now,
        }],
        checkouts: vec![WorkspaceCheckout {
            workspace_id: "ws_orbit".to_string(),
            repo_root: "/repos/orbit".into(),
            orbit_dir: "/repos/orbit/.orbit".into(),
            role: Some(WorkspaceCheckoutRole::Replica),
            owner_machine_id: Some("hm_owner".to_string()),
            path_overrides: vec!["/worktrees/orbit-feature".into()],
        }],
        ..Default::default()
    };

    let value = serde_json::to_value(&registry).expect("serialize registry");
    assert_eq!(value["schema_version"], WORKSPACE_REGISTRY_SCHEMA_VERSION);
    // The catalog records stable owner ids only; a machine's display name is
    // read from its own `machine.name` at use [ORB-12725].
    assert!(value.get("owner_host_ids").is_none());
    assert!(value["workspaces"][0].get("repo_root").is_none());
    assert!(value["workspaces"][0].get("orbit_dir").is_none());
    assert_eq!(value["checkouts"][0]["role"], "replica");
    assert_eq!(value["checkouts"][0]["owner_machine_id"], "hm_owner");
    assert!(value.get("publication_bindings").is_none());
}
