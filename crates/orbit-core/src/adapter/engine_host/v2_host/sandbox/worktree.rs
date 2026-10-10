use std::path::{Path, PathBuf};

use crate::OrbitRuntime;

/// The active job-run worktree containing `subprocess_cwd`: the direct child
/// of `<workspace>/.orbit/state/worktrees/` it canonicalizes beneath, or
/// `None` for any cwd outside that prefix. macOS anchors a writer profile
/// from that cwd at this root, so the policy's own grants and denies apply
/// inside the worktree as authored; Linux marks the run as a managed worktree.
pub(super) fn active_worktree_root(
    runtime: &OrbitRuntime,
    subprocess_cwd: &Path,
) -> Option<PathBuf> {
    let cwd = subprocess_cwd
        .canonicalize()
        .unwrap_or_else(|_| subprocess_cwd.to_path_buf());
    let workspace_orbit = runtime
        .paths()
        .orbit_dir
        .canonicalize()
        .unwrap_or_else(|_| runtime.paths().orbit_dir.clone());
    let worktrees_root = workspace_orbit.join("state").join("worktrees");
    // Require the cwd to live strictly under `…/.orbit/state/worktrees/`.
    // A bare `worktrees` cwd would anchor at the entire registry; one path
    // segment deeper restricts the anchor to a single jrun subtree.
    let relative = cwd.strip_prefix(&worktrees_root).ok()?;
    let mut components = relative.components();
    let first = components.next()?;
    Some(worktrees_root.join(first.as_os_str()))
}

/// Recovery owns a detached checkout directly beneath its host-owned state
/// directory. Recognize only that exact checkout, never the pool or an alias
/// outside it, so sandbox preparation and post-run checks stay run-scoped.
pub(super) fn recovery_checkout_root(
    runtime: &OrbitRuntime,
    subprocess_cwd: &Path,
) -> Option<PathBuf> {
    let cwd = subprocess_cwd.canonicalize().ok()?;
    let checkouts = runtime
        .paths()
        .state_dir
        .join("recovery-checkouts")
        .canonicalize()
        .ok()?;
    (cwd.parent() == Some(checkouts.as_path())).then_some(cwd)
}
