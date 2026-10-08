//! Trusted authority roots, symlink refusal and filesystem protections.

use std::fs;
use std::path::{Component, Path, PathBuf};

use orbit_common::OrbitError;
use orbit_types::policy::ResolvedFsProfile;
use rusqlite::Connection;

use super::error::path_error;

/// Directory holding the authority database, relative to the global root.
pub(super) const AUTHORITY_DIR: &str = "state/recovery-authority";

/// Database file name. Its `-wal` and `-shm` sidecars are created beside it and
/// are covered by the same protected root.
pub(super) const AUTHORITY_DB: &str = "authority.db";

/// Owner-only permissions for the authority root and its database. These do not
/// confine a same-UID leaf on their own; they keep the record off any shared or
/// group-readable path.
#[cfg(unix)]
const AUTHORITY_DIR_MODE: u32 = 0o700;
#[cfg(unix)]
const AUTHORITY_FILE_MODE: u32 = 0o600;

/// The protected root, validated before any filesystem effect.
///
/// Order matters: the trusted root is established first, and only then is the
/// authority tree created beneath it. Creating first and canonicalizing
/// afterwards would erase exactly the aliases the check is looking for, so a
/// symlink planted below the root would be followed and then declared clean.
pub(super) fn authority_root(global_root: &Path) -> Result<PathBuf, OrbitError> {
    let trusted = validated_authority_global_root(global_root)?;
    create_authority_root_under(&trusted)
}

/// Establish the trusted root the authority tree may be built under.
///
/// The configured global root reaches this module from `~/.orbit` or from a
/// managed run's registry locator, so it is untrusted input: it is required to
/// be an absolute, traversal-free path that already exists as a directory, and
/// it is resolved *without writing anything*. Aliasing in the configured root
/// itself is the operator's — a symlinked `$HOME` or a macOS `/var` prefix is a
/// supported layout, and anyone able to redirect the global root already owns
/// the run store the authority exists to outrank — so it is resolved once here
/// and the canonical result becomes the trusted root every later join is
/// anchored to.
pub(super) fn validated_authority_global_root(global_root: &Path) -> Result<PathBuf, OrbitError> {
    if !global_root.is_absolute() {
        return Err(OrbitError::InvalidInput(format!(
            "recovery authority root `{}` must be absolute",
            global_root.display()
        )));
    }
    if !global_root
        .components()
        .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
    {
        return Err(OrbitError::InvalidInput(format!(
            "recovery authority root `{}` must not contain traversal components",
            global_root.display()
        )));
    }

    let trusted = global_root
        .canonicalize()
        .map_err(|error| path_error("resolve recovery authority root", global_root, error))?;
    if !trusted.is_dir() {
        return Err(OrbitError::InvalidInput(format!(
            "recovery authority root `{}` must be an existing directory",
            global_root.display()
        )));
    }
    Ok(trusted)
}

/// Create `state/recovery-authority` one component at a time under `trusted`.
///
/// `create_dir_all` would happily follow a symlink standing in for `state` and
/// leave the authority in a directory the planter controls. `create_dir` never
/// writes through an existing entry, so each component is created inside a
/// parent this walk has already confirmed is a real directory, and is then
/// re-inspected without following links before it becomes the next parent.
fn create_authority_root_under(trusted: &Path) -> Result<PathBuf, OrbitError> {
    let mut root = trusted.to_path_buf();
    for component in Path::new(AUTHORITY_DIR).components() {
        root.push(component);
        match fs::create_dir(&root) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(path_error("create recovery authority root", &root, error));
            }
        }

        let Some(metadata) = unfollowed_metadata(&root)? else {
            return Err(path_error(
                "inspect recovery authority path",
                &root,
                std::io::Error::from(std::io::ErrorKind::NotFound),
            ));
        };
        if metadata.file_type().is_symlink() {
            return Err(refuse_symlinked(&root));
        }
        if !metadata.is_dir() {
            return Err(OrbitError::PolicyDenied(format!(
                "recovery authority refuses non-directory path `{}`",
                root.display()
            )));
        }
    }
    Ok(root)
}

/// Refuse the authority database and its sidecars when any of them is a link.
///
/// A symlinked database file would let the certificate be read from, and
/// written to, a file outside the protected root while every directory on the
/// way there still looks correct. A missing sidecar is ordinary: SQLite creates
/// them on first open, and a database written before sidecars were kept may
/// have none.
pub(super) fn refuse_symlinked_authority_files(root: &Path) -> Result<(), OrbitError> {
    for name in authority_file_names() {
        let file = root.join(name);
        if unfollowed_metadata(&file)?.is_some_and(|metadata| metadata.file_type().is_symlink()) {
            return Err(refuse_symlinked(&file));
        }
    }
    Ok(())
}

/// Keep the `-wal` and `-shm` sidecars when the last connection closes.
///
/// Workers read this database read-only from inside their sandbox, which may
/// read the authority root but never write it. SQLite opens a read-only WAL
/// database without write access only when the shared-memory sidecar already
/// exists, and by default the last closing connection deletes both sidecars.
/// When no host connection was open, a worker's binding lookup then failed
/// with "unable to open database file". Persistent sidecars make the lookup
/// independent of whether the host happens to hold the database open.
pub(super) fn persist_wal_sidecars(connection: &Connection) -> Result<(), OrbitError> {
    let mut enabled: std::ffi::c_int = 1;
    // SAFETY: the handle is live for the borrow of `connection`, and
    // SQLITE_FCNTL_PERSIST_WAL reads and writes exactly one `int`.
    let code = unsafe {
        rusqlite::ffi::sqlite3_file_control(
            connection.handle(),
            c"main".as_ptr(),
            rusqlite::ffi::SQLITE_FCNTL_PERSIST_WAL,
            (&raw mut enabled).cast(),
        )
    };
    if code != rusqlite::ffi::SQLITE_OK {
        return Err(OrbitError::Execution(format!(
            "keep recovery authority WAL sidecars: sqlite result code {code}"
        )));
    }
    Ok(())
}

/// The database and the two SQLite sidecars that share its protected root.
fn authority_file_names() -> [String; 3] {
    [
        AUTHORITY_DB.to_string(),
        format!("{AUTHORITY_DB}-wal"),
        format!("{AUTHORITY_DB}-shm"),
    ]
}

/// `path`'s own metadata, never the metadata of a symlink's target. `None` when
/// nothing is there.
fn unfollowed_metadata(path: &Path) -> Result<Option<fs::Metadata>, OrbitError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(path_error("inspect recovery authority path", path, error)),
    }
}

pub(super) fn refuse_symlinked(path: &Path) -> OrbitError {
    OrbitError::PolicyDenied(format!(
        "recovery authority refuses symlinked path `{}`",
        path.display()
    ))
}

#[cfg(unix)]
pub(super) fn restrict_permissions(root: &Path) -> Result<(), OrbitError> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(root, fs::Permissions::from_mode(AUTHORITY_DIR_MODE))
        .map_err(|error| path_error("restrict recovery authority root", root, error))?;
    for name in authority_file_names() {
        let file = root.join(name);
        if file.exists() {
            fs::set_permissions(&file, fs::Permissions::from_mode(AUTHORITY_FILE_MODE))
                .map_err(|error| path_error("restrict recovery authority file", &file, error))?;
        }
    }
    Ok(())
}

#[cfg(not(unix))]
pub(super) fn restrict_permissions(_root: &Path) -> Result<(), OrbitError> {
    Ok(())
}

/// Deny the authority root to a sandboxed leaf.
///
/// No convenience grant names this root today, so this is a tripwire rather
/// than the only barrier: appended after every other grant, it keeps a future
/// broadening of `<global>` grants from silently reopening the store. The
/// subtree form covers `authority.db` together with its `-wal` and `-shm`
/// sidecars, and Bubblewrap binds each writable ancestor of a deny so the
/// directory cannot be renamed aside and replaced.
pub(crate) fn append_recovery_authority_denies(
    global_root: &Path,
    resolved: &mut ResolvedFsProfile,
) -> Result<(), OrbitError> {
    let root = authority_root(global_root)?;
    let deny = format!("!{}/**", root.display());
    if !resolved.modify.iter().any(|rule| rule == &deny) {
        resolved.modify.push(deny);
    }
    Ok(())
}
