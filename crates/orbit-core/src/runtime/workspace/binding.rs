//! The registry-neutral workspace binding a runtime is constructed with.

use std::path::PathBuf;

use orbit_common::OrbitError;
use orbit_store::workspace_id_for_orbit_dir;
use orbit_types::workflow::{ShipMode, resolved_ship_mode};
use orbit_types::workspace::{Workspace, WorkspaceCheckout};

/// Registry-neutral metadata supplied by a higher-level workspace catalog.
///
/// `orbit-core` can construct a runtime without this binding for standalone
/// compatibility. Multi-host composition supplies it explicitly so runtime
/// path and ship-mode decisions do not have to reopen a registry owned by a
/// higher feature crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRuntimeBinding {
    /// Logical catalog ID (`ws_*`). Nested managed CLI/MCP calls use this
    /// selector instead of rediscovering ownership from a linked-worktree cwd.
    pub logical_workspace_id: String,
    /// Task-store partition id: the checkout identity from
    /// `.orbit/config.yaml` that the task registry partitions task bundles by
    /// (`workspace_bindings.workspace_id`). A different namespace from
    /// `logical_workspace_id` above, which the workspace registry mints, and
    /// the two genuinely differ for any checkout bound before `workspace init`
    /// supplied an id (L-0098).
    pub task_partition_id: String,
    /// Registered owner of the logical workspace. Automation resolves the
    /// default owner of a delivery definition from it, so an unambiguously
    /// owned workspace needs no redundant per-definition configuration.
    /// Absent on standalone registries that predate machine identity.
    pub owner_machine_id: Option<String>,
    pub repo_root: PathBuf,
    pub ship_mode: ShipMode,
    /// Registered integration branch; absent for standalone runtimes.
    pub base_branch: Option<String>,
}

/// Build the neutral Core binding for one registered local checkout.
pub fn workspace_runtime_binding(
    workspace: &Workspace,
    checkout: &WorkspaceCheckout,
) -> Result<WorkspaceRuntimeBinding, OrbitError> {
    Ok(WorkspaceRuntimeBinding {
        logical_workspace_id: workspace.id.clone(),
        task_partition_id: workspace_id_for_orbit_dir(&checkout.orbit_dir)?,
        owner_machine_id: workspace.owner_machine_id.clone(),
        repo_root: checkout.repo_root.clone(),
        ship_mode: resolved_ship_mode(workspace),
        base_branch: Some(workspace.base_branch.clone()),
    })
}
