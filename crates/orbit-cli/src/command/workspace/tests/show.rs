use std::path::PathBuf;

use chrono::Utc;
use orbit_types::workspace::{
    Workspace, WorkspaceCheckout, WorkspaceCheckoutRole, WorkspaceStatus,
};

use super::super::show::{format_workspace_show, workspace_show_json};

#[test]
fn workspace_show_json_populates_owner_machine_id_for_owned_workspace() {
    let now = Utc::now();
    let workspace = Workspace {
        id: "ws_nebula".to_string(),
        name: "nebula".to_string(),
        owner_machine_id: Some("hm_ba054a1a8fbfb914".to_string()),
        git_remote: None,
        ship_mode: Some("pr".to_string()),
        base_branch: "main".to_string(),
        status: WorkspaceStatus::Active,
        created_at: now,
        updated_at: now,
    };
    let checkout = WorkspaceCheckout::owner(
        "ws_nebula".to_string(),
        PathBuf::from("/home/user/nebula"),
        PathBuf::from("/home/user/nebula/.orbit"),
    );

    let json = workspace_show_json(&workspace, &checkout);
    assert_eq!(json["workspace"]["owner_machine_id"], "hm_ba054a1a8fbfb914");
    assert_eq!(json["checkout"]["owner_machine_id"], "hm_ba054a1a8fbfb914");
    assert_ne!(
        json["checkout"]["owner_machine_id"],
        serde_json::Value::Null
    );

    let text = format_workspace_show(&workspace, &checkout);
    assert!(text.contains("owner:       hm_ba054a1a8fbfb914"));
    assert!(text.contains("role:        owner"));
    assert!(!text.contains("owner_mirror:"));
}

#[test]
fn workspace_show_json_preserves_replica_owner_machine_id() {
    let now = Utc::now();
    let workspace = Workspace {
        id: "ws_replica".to_string(),
        name: "replica".to_string(),
        owner_machine_id: Some("hm_primary_owner".to_string()),
        git_remote: None,
        ship_mode: Some("pr".to_string()),
        base_branch: "main".to_string(),
        status: WorkspaceStatus::Active,
        created_at: now,
        updated_at: now,
    };
    let checkout = WorkspaceCheckout {
        workspace_id: "ws_replica".to_string(),
        repo_root: PathBuf::from("/home/user/replica"),
        orbit_dir: PathBuf::from("/home/user/replica/.orbit"),
        role: Some(WorkspaceCheckoutRole::Replica),
        owner_machine_id: Some("hm_replica_mirror".to_string()),
        path_overrides: Vec::new(),
    };

    let json = workspace_show_json(&workspace, &checkout);
    assert_eq!(json["workspace"]["owner_machine_id"], "hm_primary_owner");
    assert_eq!(json["checkout"]["owner_machine_id"], "hm_replica_mirror");

    let text = format_workspace_show(&workspace, &checkout);
    assert!(text.contains("owner:       hm_primary_owner"));
    assert!(text.contains("role:        replica"));
    assert!(text.contains("owner_mirror: hm_replica_mirror"));
}
