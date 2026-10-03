//! Capability classes: the locked tool-to-class mapping and destination-side
//! refusal.

use super::super::capability::{CapabilityClasses, McpToolClass, ensure_tool_class_held};
use orbit_common::OrbitError;
use orbit_types::workspace::{Workspace, WorkspaceCheckout, WorkspaceCheckoutRole};
use std::path::PathBuf;

/// The behavior rule from the federated spec, locked tool by tool over the
/// whole advertised surface [ORB-11012].
const ADVERTISED_TOOL_CLASSES: &[(&str, McpToolClass)] = &[
    ("orbit.auto_task.add", McpToolClass::ControlPlane),
    ("orbit.auto_task.list", McpToolClass::ControlPlane),
    ("orbit.auto_task.mint", McpToolClass::ControlPlane),
    ("orbit.auto_task.update", McpToolClass::ControlPlane),
    ("orbit.command.exec", McpToolClass::Execute),
    ("orbit.workflow.auto", McpToolClass::Execute),
    ("orbit.routine.control", McpToolClass::Execute),
    ("orbit.pipeline.invoke", McpToolClass::Execute),
    ("orbit.agent.invoke", McpToolClass::Execute),
    ("orbit.friction.add", McpToolClass::ControlPlane),
    ("orbit.friction.update", McpToolClass::ControlPlane),
    ("orbit.search", McpToolClass::ControlPlane),
    ("orbit.task.add", McpToolClass::ControlPlane),
    ("orbit.task.artifact.get", McpToolClass::ControlPlane),
    ("orbit.task.artifact.put", McpToolClass::ControlPlane),
    ("orbit.task.list", McpToolClass::ControlPlane),
    ("orbit.task.show", McpToolClass::ControlPlane),
    ("orbit.task.update", McpToolClass::ControlPlane),
    ("orbit.workflow.run.list", McpToolClass::Execute),
    ("orbit.workflow.run.resume", McpToolClass::Execute),
    ("orbit.workflow.run.show", McpToolClass::Execute),
    ("orbit.workflow.ship", McpToolClass::ControlPlane),
    ("orbit.workspace.list", McpToolClass::Unclassified),
];

/// The classifier is a function over the live surface, so a tool added to the
/// registry without a class assignment fails here instead of silently becoming
/// unclassified and unrefusable.
#[test]
fn the_locked_mapping_covers_exactly_the_advertised_surface() {
    let advertised = crate::canonical_mcp_tool_definitions()
        .expect("canonical MCP definitions")
        .into_iter()
        .map(|definition| definition.schema.name)
        .collect::<std::collections::BTreeSet<_>>();
    let locked = ADVERTISED_TOOL_CLASSES
        .iter()
        .map(|(name, _)| (*name).to_string())
        .collect::<std::collections::BTreeSet<_>>();

    assert_eq!(advertised, locked);
}

/// A replica answers for its own checkout, so it must not answer an owner
/// question about receipts, claims, or the ship contract admission resolves
/// [ORB-12495].
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

    let refused = ensure_tool_class_held("orbit.task.add", held)
        .expect_err("a replica is not the control plane");
    assert!(
        matches!(&refused, OrbitError::CapabilityRefused(message) if message.contains("control_plane")),
        "{refused}"
    );

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
