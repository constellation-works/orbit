use orbit_common::OrbitError;
use orbit_types::workspace::WorkspacePaths;

use crate::OrbitRuntime;

pub(crate) fn current_repo_root(runtime: &OrbitRuntime) -> Result<String, OrbitError> {
    Ok(runtime
        .context
        .paths()
        .repo_root
        .to_string_lossy()
        .to_string())
}

/// Workspace `.orbit` stores a nested Orbit process writes on Codex's behalf.
const CODEX_WORKSPACE_STORES: [&str; 5] = [
    "tasks",
    "frictions",
    "state/audit",
    "state/logs",
    "state/job-runs",
];

/// Global `~/.orbit` stores a nested Orbit process or a build writes.
const CODEX_GLOBAL_STORES: [&str; 4] = ["tasks", "state/audit", "state/logs", "cache"];

/// Codex `--add-dir` roots for its `workspace-write` sandbox.
///
/// Path-shaped runtime stores only, never a whole `.orbit` root: the roots
/// hold the host clock's definitions, crew and sandbox config, resources,
/// other runs' worktrees and the `orbit` binary itself. An OS sandbox mirrors
/// these as side-write roots, so this list is also its Codex write surface.
/// Absent stores are left out rather than handed to Codex as missing roots.
pub(crate) fn codex_workspace_write_writable_dirs(paths: &WorkspacePaths) -> Vec<String> {
    let stores = CODEX_WORKSPACE_STORES
        .iter()
        .map(|relative| paths.orbit_dir.join(relative))
        .chain(
            CODEX_GLOBAL_STORES
                .iter()
                .map(|relative| paths.global_dir.join(relative)),
        );
    let mut dirs = Vec::new();
    for dir in stores.filter(|dir| dir.is_dir()) {
        let dir = dir.to_string_lossy().into_owned();
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    dirs
}
