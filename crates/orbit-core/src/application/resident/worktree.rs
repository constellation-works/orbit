//! The managed worktree a resident cycle's agent launches in.
//!
//! The cycle is a reviewer-profile activity that acts through Orbit tools,
//! never through files, so it used to launch in the registered checkout.
//! On Linux that direct launch cannot enforce a non-subtree `denyModify`
//! such as `**/.env`: Bubblewrap can only mount the matches that exist at
//! spawn, and a post-run guard is what catches a match created afterwards.
//! The sandbox only carries that guard for a managed worktree, so a direct
//! resident launch was refused before the agent ran [ORB-24301].
//!
//! `resident_observe` therefore creates a detached worktree of the
//! workspace `HEAD` under `.orbit/state/worktrees/resident-<run_id>` — the
//! root sandbox resolution already recognizes as a managed worktree — and
//! the brief names it as the cycle's `workspace_path`. `resident_finalize`
//! removes it; a dead predecessor's worktree is removed when its cycle is
//! recovered. Nothing the agent writes there is committed or delivered.

use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::fs::git::{GIT_CHECKOUT_TIMEOUT, run_git, run_git_within};

use crate::OrbitRuntime;

/// The managed worktree for `run_id`'s cycle, or `None` when the run id
/// cannot name a single directory component.
pub(crate) fn resident_cycle_worktree_path(worktrees_dir: &Path, run_id: &str) -> Option<PathBuf> {
    if run_id.is_empty()
        || !run_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }
    Some(worktrees_dir.join(format!("resident-{run_id}")))
}

impl OrbitRuntime {
    /// Create (or reuse, for a resumed run) this cycle's managed worktree
    /// at the workspace `HEAD`.
    ///
    /// Best-effort: a workspace whose `HEAD` cannot be checked out (not a
    /// Git repository, no commit yet) yields `None` and the cycle launches
    /// as it did before, in the registered checkout. On a Git workspace —
    /// the launch that was refused — the worktree is created and the
    /// sandbox marks the launch managed.
    pub(crate) fn create_resident_cycle_worktree(&self, run_id: &str) -> Option<PathBuf> {
        let created = self.create_resident_cycle_worktree_inner(run_id);
        match created {
            Ok(path) => Some(path),
            Err(error) => {
                tracing::warn!(
                    run_id,
                    %error,
                    "resident cycle launches without a managed worktree"
                );
                None
            }
        }
    }

    fn create_resident_cycle_worktree_inner(&self, run_id: &str) -> Result<PathBuf, OrbitError> {
        let path =
            resident_cycle_worktree_path(&self.paths().worktrees_dir, run_id).ok_or_else(|| {
                OrbitError::InvalidInput(format!("run id '{run_id}' cannot name a worktree"))
            })?;
        // A resumed run re-enters its live cycle: keep the worktree it
        // already holds, scratch included, rather than replacing it.
        if path.join(".git").exists() {
            return path
                .canonicalize()
                .map_err(|error| OrbitError::Execution(format!("resolve worktree: {error}")));
        }
        let head = run_git(&self.paths().repo_root, &["rev-parse", "HEAD"])
            .map_err(|error| OrbitError::Execution(error.to_string()))?;
        if !head.success {
            return Err(OrbitError::Execution(format!(
                "resolve workspace HEAD for the cycle worktree: {}",
                head.stderr.trim()
            )));
        }
        let head = head.stdout.trim();
        self.remove_resident_cycle_worktree(run_id)?;
        if let Some(parent) = path.parent() {
            orbit_common::fs::io::create_private_dir_all(parent).map_err(|error| {
                OrbitError::Execution(format!(
                    "create worktree directory {}: {error}",
                    parent.display()
                ))
            })?;
        }
        let target = path.to_string_lossy().to_string();
        let output = run_git_within(
            &self.paths().repo_root,
            &["worktree", "add", "--detach", "--quiet", &target, head],
            GIT_CHECKOUT_TIMEOUT,
        )
        .map_err(|error| OrbitError::Execution(error.to_string()))?;
        if !output.success {
            return Err(OrbitError::Execution(format!(
                "create cycle worktree {target} at {head}: {}",
                output.stderr.trim()
            )));
        }
        // Bubblewrap needs an existing inode for the checkout's denied
        // `.orbit` root, as for a recovery checkout. Keep it empty; the
        // launcher prepares scratch there and no stores are copied.
        let denied_root = path.join(".orbit");
        if !denied_root.exists() {
            orbit_common::fs::io::create_private_dir(&denied_root).map_err(|error| {
                OrbitError::Execution(format!(
                    "prepare cycle worktree deny root {}: {error}",
                    denied_root.display()
                ))
            })?;
        }
        path.canonicalize()
            .map_err(|error| OrbitError::Execution(format!("resolve worktree {target}: {error}")))
    }

    /// Remove `run_id`'s cycle worktree, if it exists.
    pub(crate) fn remove_resident_cycle_worktree(&self, run_id: &str) -> Result<(), OrbitError> {
        let Some(path) = resident_cycle_worktree_path(&self.paths().worktrees_dir, run_id) else {
            return Ok(());
        };
        if !path.exists() {
            return Ok(());
        }
        let target = path.to_string_lossy().to_string();
        let output = run_git_within(
            &self.paths().repo_root,
            &["worktree", "remove", "--force", &target],
            GIT_CHECKOUT_TIMEOUT,
        )
        .map_err(|error| OrbitError::Execution(error.to_string()))?;
        if output.success {
            return Ok(());
        }
        // Not a registered worktree (an interrupted `worktree add`): the
        // directory is this cycle's own scratch, so remove it and prune.
        std::fs::remove_dir_all(&path).map_err(|error| {
            OrbitError::Execution(format!("remove cycle worktree {target}: {error}"))
        })?;
        let _ = run_git(&self.paths().repo_root, &["worktree", "prune"]);
        Ok(())
    }

    /// Best-effort removal for a superseded cycle: a leftover costs disk,
    /// never correctness, so a failure is recorded instead of raised.
    pub(crate) fn discard_resident_cycle_worktree(&self, run_id: &str) {
        if let Err(error) = self.remove_resident_cycle_worktree(run_id) {
            tracing::warn!(run_id, %error, "resident cycle worktree removal failed");
        }
    }
}
