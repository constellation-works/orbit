//! The federated workspace descriptor — the pinned wire shape of the mux list.
//!
//! A descriptor is a v1 workspace record as one destination reported it, plus
//! exactly six federated keys: `selector`, `machine_name`, `machine_id`,
//! `reachability`, `checkout_health`, and `capabilities`. Those names are
//! protocol, so reachability and checkout presence stay two separate fields
//! rather than one merged `health`, and `machine_id` sits on every descriptor
//! instead of on the envelope the way v1 keys it.

use orbit_types::workspace::{Workspace, WorkspaceStatus};
use serde::Serialize;

use super::config::{Destination, MachineQualifiedSelector};

/// Whether the configured destination answered this call's live probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reachability {
    Reachable,
    Unreachable,
}

/// Repo-root presence for one workspace at its destination.
///
/// Kept separate from [`Reachability`]: a machine that never answered tells us
/// nothing about its checkouts, and collapsing the two would make that silence
/// indistinguishable from a broken repo root.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckoutHealth {
    Active,
    Invalid,
    /// The destination could not be probed, so its checkout state is unknown.
    Unknown,
}

/// A tool class a destination advertises for one workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    ControlPlane,
    Execute,
}

/// One row of the federated list.
///
/// `workspace` is flattened rather than nested so a descriptor stays a v1
/// workspace record with federated keys added, which is what the spec pins.
/// It is absent only for a destination that could not be probed: there is no
/// workspace to describe, and inventing one would let a caller address a route
/// that was never observed.
#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceDescriptor {
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    workspace: Option<Workspace>,
    /// Structured, caller-uninterpreted route token. `None` when no workspace
    /// was observed, or when the destination reported a workspace ID that is
    /// not machine-qualifiable.
    selector: Option<String>,
    /// Destination display identity. Local destinations use the accepting
    /// machine's `machine.name`. Remotes use the operator's configured SSH
    /// target: the v1 discovery envelope carries no display name.
    machine_name: String,
    /// Destination stable identity, as pinned by the operator's config.
    machine_id: String,
    reachability: Reachability,
    checkout_health: CheckoutHealth,
    capabilities: Vec<Capability>,
}

impl WorkspaceDescriptor {
    /// Project one workspace a reachable destination reported.
    pub(super) fn reachable(destination: &Destination, workspace: Workspace) -> Self {
        let checkout_health = match workspace.status {
            WorkspaceStatus::Active => CheckoutHealth::Active,
            WorkspaceStatus::Invalid => CheckoutHealth::Invalid,
        };
        let capabilities = advertised_capabilities(
            &destination.machine_id,
            workspace.owner_machine_id.as_deref(),
        );
        Self {
            selector: selector_for(&destination.machine_id, &workspace.id),
            machine_name: destination.machine_name_display().to_string(),
            machine_id: destination.machine_id.clone(),
            reachability: Reachability::Reachable,
            checkout_health,
            capabilities,
            workspace: Some(workspace),
        }
    }

    /// The placeholder row for a destination that answered nothing.
    ///
    /// Every configured destination contributes at least one row: omitting a
    /// down machine would hide it from the caller and turn each later routed
    /// call into a stale-route surprise.
    pub(super) fn unreachable(destination: &Destination) -> Self {
        Self {
            workspace: None,
            selector: None,
            machine_name: destination.machine_name_display().to_string(),
            machine_id: destination.machine_id.clone(),
            reachability: Reachability::Unreachable,
            checkout_health: CheckoutHealth::Unknown,
            capabilities: Vec::new(),
        }
    }

    /// The row for a destination that answered but reported no workspaces.
    ///
    /// The machine is up and its checkouts are simply not visible to
    /// discovery, which is a different fact from "the machine never answered".
    pub(super) fn workspaceless(destination: &Destination) -> Self {
        Self {
            workspace: None,
            selector: None,
            machine_name: destination.machine_name_display().to_string(),
            machine_id: destination.machine_id.clone(),
            reachability: Reachability::Reachable,
            checkout_health: CheckoutHealth::Unknown,
            capabilities: Vec::new(),
        }
    }
}

/// Which classes the destination advertises for a workspace.
///
/// This is a hint derived from identity alone; the destination's own Core is
/// the enforcement boundary and may still refuse a class it appears to hold.
/// A workspace whose record omits `owner_machine_id` predates machine identity and
/// is therefore never a control-plane authority.
fn advertised_capabilities(
    destination_machine_id: &str,
    owner_machine_id: Option<&str>,
) -> Vec<Capability> {
    match owner_machine_id {
        Some(owner) if owner == destination_machine_id => {
            vec![Capability::ControlPlane, Capability::Execute]
        }
        _ => vec![Capability::Execute],
    }
}

/// Build the route token, or `None` for a workspace ID this encoding cannot
/// name. A legacy standalone registry may hold an ID that is not `ws_*`; that
/// workspace is still listed, just not addressable by a federated selector.
fn selector_for(machine_id: &str, workspace_id: &str) -> Option<String> {
    format!("{machine_id}/{workspace_id}")
        .parse::<MachineQualifiedSelector>()
        .ok()
        .map(|selector| selector.to_string())
}
