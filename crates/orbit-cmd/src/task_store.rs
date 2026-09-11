//! Task-store partition paths, shared by `workspace teardown` and `doctor`'s
//! orphan-partition check/fix [ORB-12109].
//!
//! `orbit-store` owns the per-workspace bundle layout
//! (`<global_root>/tasks/workspaces/<workspace_id>/`) but knows nothing
//! about the workspace registry; this module is the composition seam that
//! lets a caller resolve or remove one workspace's partition without
//! reaching around `orbit-store` from `orbit-cli`.

use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
pub use orbit_store::maintenance::task_registry::task_workspaces_dir;

/// Path to one workspace's task-store partition under
/// `<global_root>/tasks/workspaces/<workspace_id>/`.
pub fn task_store_partition_path(global_root: &Path, workspace_id: &str) -> PathBuf {
    task_workspaces_dir(global_root).join(workspace_id)
}

/// Delete one workspace's task-store partition if present. Returns whether
/// anything was removed.
pub fn remove_task_store_partition(
    global_root: &Path,
    workspace_id: &str,
) -> Result<bool, OrbitError> {
    let path = task_store_partition_path(global_root, workspace_id);
    if !path.is_dir() {
        return Ok(false);
    }
    std::fs::remove_dir_all(&path).map_err(|error| {
        OrbitError::Io(format!("remove task store {}: {error}", path.display()))
    })?;
    Ok(true)
}
