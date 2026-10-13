//! Destination capability classes through the exported classifier.

use std::path::PathBuf;

use orbit_common::OrbitError;
use orbit_mcp::federated::{CapabilityClasses, McpToolClass, ensure_tool_class_held};
use orbit_types::workspace::{Workspace, WorkspaceCheckout, WorkspaceCheckoutRole};

/// A replica answers for its own checkout, so it must not answer an owner
/// question about receipts, claims, or the ship contract admission resolves.
#[test]
fn a_replica_refuses_the_distributed_drain_read_only_surface() {
    let held = CapabilityClasses::new(false, true);

    for tool in [
        "orbit.drain.probe",
        "orbit.drain.receipt.lookup",
        "orbit.task.pull",
        "orbit.drain.claim.bind",
        "orbit.drain.claim.settle",
    ] {
        let error = ensure_tool_class_held(tool, held).expect_err("replica refuses");
        assert!(
            matches!(error, OrbitError::CapabilityRefused(_)),
            "{tool}: {error}"
        );
        assert!(error.to_string().contains("control_plane"), "{tool}");
    }
}

#[test]
fn a_replica_refuses_control_plane_and_runs_execute_class_tools() {
    let held = CapabilityClasses::for_checkout(
        &workspace_record(Some("hm_owner")),
        &checkout_record(Some(WorkspaceCheckoutRole::Replica)),
    );

    for tool in ["orbit.task.add", "orbit.task.review_reset"] {
        let refused =
            ensure_tool_class_held(tool, held).expect_err("a replica is not the control plane");
        assert!(
            matches!(&refused, OrbitError::CapabilityRefused(message) if message.contains("control_plane")),
            "{tool}: {refused}"
        );
    }

    for allowed in [
        "orbit.workflow.run.show",
        "orbit.workflow.auto",
        "orbit.command.exec",
    ] {
        ensure_tool_class_held(allowed, held).expect(allowed);
    }
}

/// A standalone registry predating host identity is not a control-plane
/// authority, whatever role its checkout claims.
#[test]
fn a_workspace_without_an_owner_machine_id_does_not_advertise_control_plane() {
    for role in [None, Some(WorkspaceCheckoutRole::Owner)] {
        let held = CapabilityClasses::for_checkout(&workspace_record(None), &checkout_record(role));

        assert!(!held.holds(McpToolClass::ControlPlane), "{role:?}");
        assert!(held.holds(McpToolClass::Execute), "{role:?}");
        assert!(
            ensure_tool_class_held("orbit.task.update", held).is_err(),
            "{role:?}"
        );
    }
}

fn workspace_record(owner_machine_id: Option<&str>) -> Workspace {
    let now = chrono::Utc::now();
    Workspace {
        id: "ws_orbit".to_string(),
        name: "orbit".to_string(),
        owner_machine_id: owner_machine_id.map(str::to_string),
        git_remote: None,
        ship_mode: None,
        base_branch: "main".to_string(),
        status: orbit_types::workspace::WorkspaceStatus::Active,
        created_at: now,
        updated_at: now,
    }
}

fn checkout_record(role: Option<WorkspaceCheckoutRole>) -> WorkspaceCheckout {
    WorkspaceCheckout {
        workspace_id: "ws_orbit".to_string(),
        repo_root: PathBuf::from("/srv/orbit"),
        orbit_dir: PathBuf::from("/srv/orbit/.orbit"),
        role,
        owner_machine_id: role
            .filter(|role| *role == WorkspaceCheckoutRole::Replica)
            .map(|_| "hm_owner".to_string()),
        path_overrides: Vec::new(),
    }
}
