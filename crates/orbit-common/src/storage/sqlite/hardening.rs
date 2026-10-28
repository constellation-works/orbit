//! Path validation and file-permission hardening for SQLite databases.

use std::fs;
use std::io;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use crate::OrbitError;

use super::errors::sqlite_path_error;

/// Leading bytes of every SQLite database file.
const DATABASE_MAGIC: &[u8] = b"SQLite format 3\0";

/// Bytes of the database header this module reads: through offsets 18 and 19,
/// the write and read file-format versions.
const DATABASE_HEADER_BYTES: u64 = 20;

/// File-format version recorded at offsets 18 and 19 while a database is in
/// WAL mode. Rollback-journal databases record `1`.
const WAL_FILE_FORMAT: u8 = 2;

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
    match crate::fs::io::create_new_private_file(path) {
        Ok(file) => {
            drop(file);
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            crate::fs::io::set_private_file_permissions(path)
                .map_err(|error| sqlite_path_error("harden", path, error))
        }
        Err(error) => Err(sqlite_path_error("create", path, error)),
    }
}

/// Restrict the database and its existing sidecars to owner-only access
/// through no-follow descriptors; see
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
    use std::os::unix::fs::PermissionsExt;

    let file = match open_read_no_follow(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(sqlite_path_error("inspect", path, error)),
    };

    let owner_only_mode = file
        .metadata()
        .map_err(|error| sqlite_path_error("inspect", path, error))?
        .permissions()
        .mode()
        & 0o700;
    file.set_permissions(fs::Permissions::from_mode(owner_only_mode))
        .map_err(|error| sqlite_path_error("harden", path, error))
}

#[cfg(not(unix))]
fn harden_existing_read_only_file(_path: &Path) -> Result<(), OrbitError> {
    Ok(())
}

/// Open a database or sidecar file for reading without traversing a final
/// symlink, so inspection stays inside the directory
/// [`validated_sqlite_path`] resolved.
#[cfg(unix)]
pub(super) fn open_read_no_follow(path: &Path) -> io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(not(unix))]
pub(super) fn open_read_no_follow(path: &Path) -> io::Result<fs::File> {
    fs::File::open(path)
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

/// Whether the database file records WAL mode in its header.
///
/// A missing or truncated file, and any file that does not carry the SQLite
/// magic, is reported as not WAL: there are no committed WAL frames to miss,
/// and SQLite gives a better diagnosis of the file itself than this check
/// could.
pub(super) fn database_uses_wal(path: &Path) -> Result<bool, OrbitError> {
    let file = match open_read_no_follow(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(sqlite_path_error("inspect", path, error)),
    };

    let mut header = Vec::with_capacity(DATABASE_HEADER_BYTES as usize);
    file.take(DATABASE_HEADER_BYTES)
        .read_to_end(&mut header)
        .map_err(|error| sqlite_path_error("read the header of", path, error))?;
    if header.len() < DATABASE_HEADER_BYTES as usize || !header.starts_with(DATABASE_MAGIC) {
        return Ok(false);
    }

    Ok(header[18] == WAL_FILE_FORMAT || header[19] == WAL_FILE_FORMAT)
}
