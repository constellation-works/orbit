//! Shared working-directory confinement for operator host tools.

use std::path::PathBuf;

use orbit_common::OrbitError;
use orbit_common::fs::cwd::confine_workspace_cwd;

use super::UNBOUND_DATA_DIR_PARTITION_ID;
use crate::OrbitRuntime;

impl OrbitRuntime {
    /// Canonicalize an explicit working directory and require it to stay
    /// inside this workspace's checkout or a linked worktree under
    /// `.orbit/state/worktrees/`.
    ///
    /// Shared by `orbit.agent.invoke` (`cwd`) and `orbit.command.exec`
    /// (`working_directory`) so those tools cannot drift.
    pub(crate) fn resolve_workspace_cwd(
        &self,
        field: &str,
        requested: &str,
    ) -> Result<PathBuf, OrbitError> {
        let workspace_id = self
            .workspace_id()
            .unwrap_or_else(|_| UNBOUND_DATA_DIR_PARTITION_ID.to_string());
        confine_workspace_cwd(
            field,
            requested,
            &workspace_id,
            &self.paths().repo_root,
            &self.linked_worktree_roots(),
        )
    }

    /// Linked worktrees live under `.orbit/state/worktrees/`. After
    /// canonicalization they may sit outside `repo_root` (when `.orbit` itself
    /// is a symlink to an external data directory). They must still remain
    /// under this workspace's canonical orbit directory, so a planted
    /// `worktrees` symlink cannot enlarge the allow-set.
    fn linked_worktree_roots(&self) -> Vec<PathBuf> {
        let Ok(orbit_dir) = self.paths().orbit_dir.canonicalize() else {
            return Vec::new();
        };
        let Ok(worktrees) = self.paths().worktrees_dir.canonicalize() else {
            return Vec::new();
        };
        if worktrees.is_dir() && worktrees.starts_with(&orbit_dir) {
            vec![worktrees]
        } else {
            Vec::new()
        }
    }
}
