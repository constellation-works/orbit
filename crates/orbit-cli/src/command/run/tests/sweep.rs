use chrono::Utc;
use orbit_core::application::task::TaskAddParams;
use orbit_core::runtime::WorkspaceRuntimeBinding;
use orbit_core::{OrbitRuntime, ShipMode as CoreShipMode, TaskStatus};
use orbit_types::workspace::{
    Workspace, WorkspaceCheckout, WorkspaceCheckoutRole, WorkspaceStatus,
};
use serde_json::json;
use tempfile::tempdir;

use super::super::sweep::{SweepReport, sweep_active_workspace};

#[test]
fn disabled_auto_ship_reports_non_empty_ready_backlog_in_text_and_json() {
    let fixture = tempdir().expect("fixture tempdir");
    let global_root = fixture.path().join("global");
    let repo_root = fixture.path().join("repo");
    let orbit_dir = repo_root.join(".orbit");
    std::fs::create_dir_all(&global_root).expect("global root");
    std::fs::create_dir_all(&orbit_dir).expect("workspace root");
    std::fs::write(
        orbit_dir.join("config.toml"),
        "[workflow]\nauto_ship = false\n",
    )
    .expect("write workspace config");
    std::fs::write(
        orbit_dir.join("config.yaml"),
        "schema_version: 1\nworkspace_id: ws_sweep_test\n",
    )
    .expect("write workspace identity");

    let binding = WorkspaceRuntimeBinding {
        logical_workspace_id: "ws_sweep_test".to_string(),
        task_partition_id: "ws_sweep_test".to_string(),
        owner_machine_id: None,
        repo_root: repo_root.clone(),
        ship_mode: CoreShipMode::Pr,
        base_branch: Some("agent-main".to_string()),
    };
    let runtime = OrbitRuntime::from_roots_with_binding(&global_root, &orbit_dir, binding)
        .expect("build fixture runtime");
    for title in ["first ready task", "second ready task"] {
        runtime
            .add_task(TaskAddParams {
                title: title.to_string(),
                description: "ready backlog fixture".to_string(),
                plan: "fixture plan".to_string(),
                status: Some(TaskStatus::Backlog),
                ..TaskAddParams::default()
            })
            .expect("add ready backlog task");
    }

    let workspace = Workspace {
        id: "ws_sweep_test".to_string(),
        name: "sweep-test".to_string(),
        owner_machine_id: None,
        git_remote: None,
        ship_mode: Some("pr".to_string()),
        base_branch: "agent-main".to_string(),
        status: WorkspaceStatus::Active,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    let checkout = WorkspaceCheckout::owner(workspace.id.clone(), repo_root, orbit_dir);

    let report =
        sweep_active_workspace(&global_root, &workspace, &checkout, CoreShipMode::Pr, true)
            .expect("run disabled ship sweep");
    let json = report.to_json();

    assert_eq!(json["action"], "skipped");
    assert_eq!(json["ready_backlog"], 2);
    assert_eq!(json["skip_reason"], "auto_ship_disabled");
    assert_eq!(json.get("reason"), None);
    assert_eq!(
        report.to_line(),
        "sweep-test: skipped (ready backlog: 2) — auto_ship_disabled"
    );
}

#[test]
fn skipped_report_keeps_ready_backlog_separate_from_skip_reason() {
    let workspace = Workspace {
        id: "ws_sweep_test".to_string(),
        name: "sweep-test".to_string(),
        owner_machine_id: None,
        git_remote: None,
        ship_mode: None,
        base_branch: "agent-main".to_string(),
        status: WorkspaceStatus::Active,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    let report = SweepReport::skipped(&workspace, "auto_ship_disabled", 2);

    assert_eq!(
        report.to_json(),
        json!({
            "workspace_id": "ws_sweep_test",
            "workspace_name": "sweep-test",
            "action": "skipped",
            "skip_reason": "auto_ship_disabled",
            "ready_backlog": 2,
            "mode": null,
            "run_id": null,
            "run_state": null,
        })
    );
}

/// [ORB-12500] The independent CLI sweep is a separate entry point from the
/// seeded routine and the `workspace_ship_pipeline` wrapper, and it stands
/// down on a replica checkout for its own reasons: backlog selection,
/// reservation and claim settlement are owner-only coordination work, and a
/// replica executes through pull instead.
///
/// It reports before reading the backlog at all, because a replica's task
/// reads are not the population this sweep would be counting.
#[test]
fn a_replica_checkout_stands_the_independent_cli_sweep_down() {
    let fixture = tempdir().expect("fixture tempdir");
    let global_root = fixture.path().join("global");
    let repo_root = fixture.path().join("repo");
    let orbit_dir = repo_root.join(".orbit");
    std::fs::create_dir_all(&global_root).expect("global root");
    std::fs::create_dir_all(&orbit_dir).expect("workspace root");
    // Opted in, so nothing but the replica role can be what stands it down.
    std::fs::write(
        orbit_dir.join("config.toml"),
        "[workflow]\nauto_ship = true\n",
    )
    .expect("write workspace config");
    std::fs::write(
        orbit_dir.join("config.yaml"),
        "schema_version: 1\nworkspace_id: ws_sweep_replica\n",
    )
    .expect("write workspace identity");

    let workspace = Workspace {
        id: "ws_sweep_replica".to_string(),
        name: "sweep-replica".to_string(),
        owner_machine_id: Some("hm_owner".to_string()),
        git_remote: None,
        ship_mode: Some("pr".to_string()),
        base_branch: "agent-main".to_string(),
        status: WorkspaceStatus::Active,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    let checkout = WorkspaceCheckout {
        role: Some(WorkspaceCheckoutRole::Replica),
        owner_machine_id: Some("hm_owner".to_string()),
        ..WorkspaceCheckout::owner(workspace.id.clone(), repo_root, orbit_dir)
    };

    let report =
        sweep_active_workspace(&global_root, &workspace, &checkout, CoreShipMode::Pr, false)
            .expect("run replica ship sweep");
    let json = report.to_json();

    assert_eq!(json["action"], "skipped");
    assert_eq!(json["skip_reason"], "replica_checkout");
    assert_eq!(json["run_id"], serde_json::Value::Null);
}
