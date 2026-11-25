//! Cursor file layout and sidecar locking for the auto-task scheduler.

use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::fs::io::{create_private_dir_all, with_exclusive_file_lock};

/// Path of the cursor state file under a workspace state dir.
pub fn cursor_state_path(state_dir: &Path) -> PathBuf {
    state_dir.join("auto-tasks.json")
}

pub(crate) fn with_cursor_lock<T>(
    path: &Path,
    op: impl FnOnce() -> Result<T, OrbitError>,
) -> Result<T, OrbitError> {
    if let Some(parent) = path.parent() {
        create_private_dir_all(parent).map_err(|error| {
            OrbitError::Io(format!(
                "create auto-tasks state dir {}: {error}",
                parent.display()
            ))
        })?;
    }
    with_exclusive_file_lock(path, "auto-task cursor", op)
}
