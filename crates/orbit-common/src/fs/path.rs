use std::path::{Path, PathBuf};

use crate::OrbitError;

/// The directory that holds `path`, with the empty parent of a bare relative
/// name (`state.json`) read as the current directory (`.`), as an atomic write
/// target does. `None` for a path with no parent, such as a root.
pub(crate) fn parent_dir_or_current(path: &Path) -> Option<&Path> {
    let parent = path.parent()?;
    Some(if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    })
}

/// Worker-facing scratch directory under a checkout: `<root>/.orbit/tmp`.
///
/// Gitignored with the rest of `.orbit/`, inside `workspace_root` so
/// `orbit.task.artifact.put` accepts it, and run-scoped in a job worktree.
pub use crate::env_vars::ORBIT_SCRATCH_DIR as ORBIT_SCRATCH_DIR_ENV;

/// Absolute path of the sanctioned worker scratch directory for `workspace_root`.
pub fn orbit_scratch_dir(workspace_root: impl AsRef<Path>) -> PathBuf {
    workspace_root.as_ref().join(".orbit").join("tmp")
}

/// Create `<workspace_root>/.orbit/tmp` and return its canonical path.
pub fn ensure_orbit_scratch_dir(workspace_root: impl AsRef<Path>) -> Result<PathBuf, OrbitError> {
    let scratch = orbit_scratch_dir(workspace_root);
    crate::fs::io::create_private_dir_all(&scratch).map_err(|error| {
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
pub fn global_orbit_dir(env: &crate::Env) -> Result<PathBuf, OrbitError> {
    Ok(home_dir(env)?.join(".orbit"))
}

/// Directory holding authenticated SSH control sockets for remote sessions.
///
/// This follows the invoking user's home even when a runtime uses a relocated
/// registry root. Sandboxed agents must hide this same directory.
pub fn ssh_socket_dir(env: &crate::Env) -> Result<PathBuf, OrbitError> {
    Ok(global_orbit_dir(env)?.join("ssh"))
}

/// Resolve the invoking user's home from a captured environment.
pub fn home_dir_from(env: &crate::Env) -> Result<PathBuf, OrbitError> {
    [env.home(), env.userprofile()]
        .into_iter()
        .flatten()
        .filter_map(std::ffi::OsStr::to_str)
        .find(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| OrbitError::WorkspaceError("cannot determine home directory".to_string()))
}

/// The invoking user's home directory.
///
/// Machine-global Orbit state hangs off this, and so do the SSH files a
/// destination's caller authorization is read against, so both resolve it the
/// same way rather than each spelling out its own fallback.
pub fn home_dir(env: &crate::Env) -> Result<PathBuf, OrbitError> {
    let home = env
        .home()
        .and_then(std::ffi::OsStr::to_str)
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            env.userprofile()
                .and_then(std::ffi::OsStr::to_str)
                .filter(|value| !value.trim().is_empty())
        })
        .ok_or_else(|| OrbitError::WorkspaceError("cannot determine home directory".to_string()))?;
    Ok(PathBuf::from(home))
}
