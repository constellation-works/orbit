//! Filesystem ownership and mode checks for the private token store.

use std::path::Path;

use orbit_common::OrbitError;

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt};

#[cfg(unix)]
/// Refuse a store or home that is not the running user's alone.
pub(super) fn require_owner_only(path: &Path, directory: bool) -> Result<(), OrbitError> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| OrbitError::Io(format!("cannot inspect {}: {error}", path.display())))?;
    let kind_ok = if directory {
        metadata.is_dir()
    } else {
        metadata.is_file()
    };
    if !kind_ok {
        return Err(refused(
            path,
            if directory {
                "is not a directory"
            } else {
                "is not a regular file"
            },
        ));
    }
    // SAFETY: `geteuid` has no preconditions and cannot fail.
    let euid = unsafe { libc::geteuid() };
    if metadata.uid() != euid {
        return Err(refused(path, "is owned by another user"));
    }
    let mode = metadata.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(OrbitError::PolicyDenied(format!(
            "refusing the lease token store: {} has mode {mode:03o}, which other users can \
             access; it must be {}",
            path.display(),
            if directory { "0700" } else { "0600" }
        )));
    }
    Ok(())
}

#[cfg(unix)]
fn refused(path: &Path, why: &str) -> OrbitError {
    OrbitError::PolicyDenied(format!(
        "refusing the lease token store: {} {why}",
        path.display()
    ))
}

#[cfg(not(unix))]
pub(super) fn require_owner_only(_path: &Path, _directory: bool) -> Result<(), OrbitError> {
    Err(OrbitError::PolicyDenied(
        "the lease token store needs a Unix host".to_string(),
    ))
}
