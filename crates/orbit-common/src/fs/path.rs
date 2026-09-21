use std::path::{Path, PathBuf};

use crate::OrbitError;

use super::selector::overlaps;

/// Worker-facing scratch directory under a checkout: `<root>/.orbit/tmp`.
///
/// Gitignored with the rest of `.orbit/`, inside `workspace_root` so
/// `orbit.task.artifact.put` accepts it, and run-scoped in a job worktree.
pub const ORBIT_SCRATCH_DIR_ENV: &str = "ORBIT_SCRATCH_DIR";

/// Absolute path of the sanctioned worker scratch directory for `workspace_root`.
pub fn orbit_scratch_dir(workspace_root: impl AsRef<Path>) -> PathBuf {
    workspace_root.as_ref().join(".orbit").join("tmp")
}

/// Create `<workspace_root>/.orbit/tmp` and return its canonical path.
pub fn ensure_orbit_scratch_dir(workspace_root: impl AsRef<Path>) -> Result<PathBuf, OrbitError> {
    let scratch = orbit_scratch_dir(workspace_root);
    std::fs::create_dir_all(&scratch).map_err(|error| {
        OrbitError::Io(format!(
            "create scratch dir '{}': {error}",
            scratch.display()
        ))
    })?;
    scratch.canonicalize().map_err(|error| {
        OrbitError::Io(format!(
            "canonicalize scratch dir '{}': {error}",
            scratch.display()
        ))
    })
}

/// Return the machine-global Orbit directory at `~/.orbit`.
pub fn global_orbit_dir() -> Result<PathBuf, OrbitError> {
    Ok(home_dir()?.join(".orbit"))
}

/// The invoking user's home directory.
///
/// Machine-global Orbit state hangs off this, and so do the SSH files a
/// destination's caller authorization is read against, so both resolve it the
/// same way rather than each spelling out its own fallback.
pub fn home_dir() -> Result<PathBuf, OrbitError> {
    let home = std::env::var("HOME")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            std::env::var("USERPROFILE")
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
        .ok_or_else(|| OrbitError::WorkspaceError("cannot determine home directory".to_string()))?;
    Ok(PathBuf::from(home))
}

/// Returns true when two task context scopes overlap on the same filesystem
/// anchor or on an ancestor/descendant boundary.
///
/// This helper accepts both legacy raw paths and canonical selector strings
/// and delegates to the shared selector overlap semantics.
pub fn workspace_relative_paths_overlap(left: &str, right: &str) -> bool {
    overlaps(left, right)
}

pub fn normalize_workspace_relative_path(path: &str) -> Option<&str> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return None;
    }

    let normalized = trimmed.trim_end_matches('/');
    if normalized.is_empty() {
        return None;
    }

    Some(normalized)
}
