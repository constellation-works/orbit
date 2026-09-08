//! Workspace-init helpers for the hub task registry.
//!
//! Task and friction tool CRUD belong to the checkout-backed runtime host.
//! This type only registers and binds a workspace in the task registry.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_store::maintenance::task_registry::{
    BindWorkspaceParams, RegisterWorkspaceParams, TaskRegistryStore, task_registry_path,
};

/// Registers and binds a logical workspace in the hub task registry.
///
/// Workspace initialization is the only remaining production caller.
pub struct HubCoordinationExecutor;

impl HubCoordinationExecutor {
    /// Registers the path-free task-registry partition for a logical workspace.
    /// Workspace initialization calls this once; identical repeats are safe.
    pub fn register_workspace(
        global_root: &Path,
        workspace_id: impl Into<String>,
        slug: impl Into<String>,
    ) -> Result<(), OrbitError> {
        let registry = TaskRegistryStore::open(&task_registry_path(global_root))?;
        registry.register_workspace(RegisterWorkspaceParams {
            workspace_id: workspace_id.into(),
            slug: slug.into(),
            repo_fingerprint: None,
        })?;
        Ok(())
    }

    /// Bind this checkout in the task registry. `--force` replaces an
    /// existing orbit-dir row so a synthetic parent(data-dir) bind cannot
    /// leave split-brain state after workspace init.
    pub fn bind_checkout(
        global_root: &Path,
        workspace_id: impl Into<String>,
        slug: impl Into<String>,
        repo_root: &Path,
        orbit_dir: &Path,
        replace_existing: bool,
    ) -> Result<(), OrbitError> {
        let registry = TaskRegistryStore::open(&task_registry_path(global_root))?;
        let params = BindWorkspaceParams {
            workspace_id: Some(workspace_id.into()),
            slug: slug.into(),
            repo_root: repo_root.to_path_buf(),
            workspace_path: repo_root.to_path_buf(),
            orbit_dir: orbit_dir.to_path_buf(),
            repo_fingerprint: None,
        };
        if replace_existing {
            registry.rebind_checkout(params)?;
        } else {
            registry.bind_workspace(params)?;
        }
        Ok(())
    }
}
