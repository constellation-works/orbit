//! Path validation and file-permission hardening for SQLite databases.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use crate::OrbitError;

use super::errors::sqlite_path_error;

/// Resolve a SQLite file path through its existing parent and reject traversal
/// and final-component symlinks before any SQLite or database-file permission
/// operation.
///
/// Orbit callers provide complete paths because the database may live in a
/// caller-selected state root. The root remains caller-owned, but path
/// traversal and symlink redirection are not part of that contract. Creating
/// missing parents preserves the existing first-open behavior; once they exist,
/// all subsequent operations use the canonical parent path.
pub(super) fn validated_sqlite_path(path: &Path) -> Result<PathBuf, OrbitError> {
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(OrbitError::InvalidInput(format!(
            "SQLite path '{}' must not contain parent-directory traversal",
            path.display()
        )));
    }

    let file_name = path.file_name().ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "SQLite path '{}' must name a database file",
            path.display()
        ))
    })?;
    let parent = path.parent().ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "SQLite path '{}' must have a parent directory",
            path.display()
        ))
    })?;

    create_private_dir_all(parent)?;
    let canonical_parent = fs::canonicalize(parent)
        .map_err(|error| sqlite_path_error("resolve parent for", parent, error))?;
    let canonical_path = canonical_parent.join(file_name);

    if !canonical_path.starts_with(&canonical_parent) {
        return Err(OrbitError::InvalidInput(format!(
            "SQLite path '{}' escapes its parent directory",
            path.display()
        )));
    }

    match fs::symlink_metadata(&canonical_path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(OrbitError::InvalidInput(format!(
                "SQLite path must not be a symlink: {}",
                path.display()
            )));
        }
        Ok(metadata) if !metadata.is_file() => {
            return Err(OrbitError::InvalidInput(format!(
                "SQLite path must be a regular file: {}",
                path.display()
            )));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(sqlite_path_error("inspect", &canonical_path, error)),
    }

    Ok(canonical_path)
}

/// Create a sensitive SQLite-adjacent state directory.
///
/// On Unix, directories created by this call are `0o700`; existing ancestors
/// are deliberately left unchanged.
pub fn create_private_dir_all(path: &Path) -> Result<(), OrbitError> {
    crate::fs::io::create_private_dir_all(path)
        .map_err(|error| sqlite_path_error("create private directory", path, error))
}

pub(super) fn prepare_private_database_file(path: &Path) -> Result<(), OrbitError> {
    // Existing databases take only the metadata/private-permission path. Do
    // not stage and fsync a new inode on every ordinary connection open.
    match fs::symlink_metadata(path) {
        Ok(_) => {
            return crate::fs::io::set_private_file_permissions(path)
                .map_err(|error| sqlite_path_error("harden", path, error));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(sqlite_path_error("inspect", path, error)),
    }
    // Close the staging data descriptor before publishing this inode at the
    // database path. A concurrent constructor can only open the published
    // path after that close; the losing publication never replaces its inode.
    match crate::fs::io::write_new_private_text(path, "") {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            crate::fs::io::set_private_file_permissions(path)
                .map_err(|error| sqlite_path_error("harden", path, error))
        }
        Err(error) => Err(sqlite_path_error("create", path, error)),
    }
}

/// Restrict the database and its existing sidecars to owner-only access
/// through no-follow path-only handles where supported; see
/// [`crate::fs::io::set_private_file_permissions`].
pub(super) fn harden_sqlite_files(path: &Path) -> Result<(), OrbitError> {
    harden_existing_file(path)?;
    for sidecar in sqlite_sidecar_paths(path) {
        harden_existing_file(&sidecar)?;
    }
    Ok(())
}

fn harden_existing_file(path: &Path) -> Result<(), OrbitError> {
    match crate::fs::io::set_private_file_permissions(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(sqlite_path_error("harden", path, error)),
    }
}

pub(super) fn harden_read_only_sqlite_files(path: &Path) -> Result<(), OrbitError> {
    harden_existing_read_only_file(path)?;
    for sidecar in sqlite_sidecar_paths(path) {
        harden_existing_read_only_file(&sidecar)?;
    }
    Ok(())
}

#[cfg(unix)]
fn harden_existing_read_only_file(path: &Path) -> Result<(), OrbitError> {
    match crate::fs::io::set_read_only_private_file_permissions(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(sqlite_path_error("harden", path, error)),
    }
}

#[cfg(not(unix))]
fn harden_existing_read_only_file(_path: &Path) -> Result<(), OrbitError> {
    Ok(())
}

pub(super) fn sqlite_sidecar_paths(path: &Path) -> [PathBuf; 2] {
    [
        path_with_suffix(path, "-wal"),
        path_with_suffix(path, "-shm"),
    ]
}

/// Metadata for an existing sidecar, or `None` when it is absent.
///
/// A symlinked or otherwise irregular sidecar is rejected: read-only opens
/// decide what to trust from these files, and SQLite would follow a link out
/// of the state directory [`validated_sqlite_path`] just checked.
pub(super) fn sidecar_metadata(path: &Path) -> Result<Option<fs::Metadata>, OrbitError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(Some(metadata)),
        Ok(metadata) if metadata.file_type().is_symlink() => Err(OrbitError::InvalidInput(
            format!("SQLite sidecar must not be a symlink: {}", path.display()),
        )),
        Ok(_) => Err(OrbitError::InvalidInput(format!(
            "SQLite sidecar must be a regular file: {}",
            path.display()
        ))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(sqlite_path_error("inspect", path, error)),
    }
}

fn path_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}
