use std::path::{Path, PathBuf};

#[cfg(target_os = "macos")]
use orbit_types::policy::ResolvedFsProfile;

use crate::OrbitRuntime;

/// Re-allow the active job-run worktree under `<workspace>/.orbit/state/worktrees/`
/// for every provider, after the policy's `denyModify .orbit/**` rule. Without
/// this, `task_pr_pipeline` runs whose subprocess cwd lives under
/// `.orbit/state/worktrees/orbit-jrun-…` cannot edit their own checkout under
/// the macOS sandbox: SBPL is last-match-wins, the broad `unrestricted` profile
/// allows `<workspace>/**` first, the global deny appends `!<workspace>/.orbit/**`
/// last, and codex was the only provider that re-asserted a writable side-root
/// after that. See T20260508-17.
///
/// Scope is deliberately narrow: only the calling subprocess's cwd is
/// re-allowed, and only when it canonicalizes to a direct child of
/// `<workspace>/.orbit/state/worktrees/`. Cwds outside that prefix yield no
/// change — we do not blanket-reallow `.orbit/**` for non-codex providers.
#[cfg(target_os = "macos")]
pub(super) fn append_active_worktree_root(
    runtime: &OrbitRuntime,
    subprocess_cwd: Option<&Path>,
    resolved: &mut ResolvedFsProfile,
) {
    let Some(cwd) = subprocess_cwd else {
        return;
    };
    let Some(worktree_root) = active_worktree_subpath(runtime, cwd) else {
        return;
    };
    // Append after the policy denies; SBPL last-match-wins re-grants writes
    // inside the active worktree without widening any path outside it.
    resolved.modify.push(worktree_root);
}

pub(super) fn active_worktree_subpath(
    runtime: &OrbitRuntime,
    subprocess_cwd: &Path,
) -> Option<String> {
    active_worktree_root(runtime, subprocess_cwd).map(|worktree| worktree.display().to_string())
}

fn active_worktree_root(runtime: &OrbitRuntime, subprocess_cwd: &Path) -> Option<PathBuf> {
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
    // A bare `worktrees` cwd would re-allow the entire registry; one path
    // segment deeper restricts the grant to a single jrun subtree.
    let relative = cwd.strip_prefix(&worktrees_root).ok()?;
    let mut components = relative.components();
    let first = components.next()?;
    Some(worktrees_root.join(first.as_os_str()))
}
