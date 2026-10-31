//! One answer to "which host directory does this granted path name".
//!
//! A granted write root is decided twice: once when it is validated against
//! the protected roots (`orbit plugin validate`, registration, and again at
//! call time), and once when the boundary is compiled and the directory is
//! created for a rule to bind to. If those two answers can differ, the check
//! and the enforcement describe different places on disk and the check stops
//! meaning anything — an existing symlink ancestor with an absent child is
//! exactly that case, because the child cannot be canonicalized and the whole
//! path then falls back to a name-only reading that ignores the link
//! [ORB-12799].
//!
//! [`physical_with_missing_tail`] (defined in `orbit_common::fs::physical`) is
//! the single resolution both sides use: the longest existing prefix resolved
//! by the kernel, with the names that do not exist yet appended to it.
//! [`create_write_root`] then materialises that same path without following a
//! link into anywhere else, and refuses when what it created does not resolve
//! to the identity that was validated.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
// Sibling modules still reach it through this path.
pub(crate) use orbit_common::fs::physical::physical_with_missing_tail;

/// Create a granted write directory at the identity
/// [`physical_with_missing_tail`] gives it, and return that identity.
///
/// A rule binds to an inode, so a grant naming a directory that does not
/// exist yet has to create it. Creation walks the missing names one at a
/// time and inspects each with `symlink_metadata`, so a link planted between
/// validation and this call is refused rather than followed, and the created
/// path is re-resolved at the end: if it does not resolve to the identity
/// that was validated, no grant is compiled for it. An existing path is
/// returned as its canonical self and never created or modified.
pub fn create_write_root(root: &Path) -> Result<PathBuf, OrbitError> {
    if let Ok(existing) = root.canonicalize() {
        return Ok(existing);
    }
    let expected = physical_with_missing_tail(root);

    let mut anchor = expected.as_path();
    let mut missing: Vec<&OsStr> = Vec::new();
    while !anchor.exists() {
        let (Some(name), Some(parent)) = (anchor.file_name(), anchor.parent()) else {
            return Err(OrbitError::InvalidInput(format!(
                "granted write directory `{}` has no existing ancestor to create it under",
                root.display()
            )));
        };
        missing.push(name);
        anchor = parent;
    }
    let mut current = anchor.canonicalize().map_err(|error| {
        OrbitError::Io(format!(
            "resolve granted write directory ancestor `{}`: {error}",
            anchor.display()
        ))
    })?;

    for name in missing.iter().rev() {
        current.push(name);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(OrbitError::InvalidInput(format!(
                    "granted write directory `{}` resolves through symbolic link `{}`; a granted \
                     write path is created where it was validated, never through a link",
                    root.display(),
                    current.display()
                )));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(OrbitError::InvalidInput(format!(
                    "granted write directory `{}` resolves through non-directory `{}`",
                    root.display(),
                    current.display()
                )));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                create_directory_component(root, &current)?;
            }
            Err(error) => {
                return Err(OrbitError::Io(format!(
                    "inspect granted write directory `{}`: {error}",
                    current.display()
                )));
            }
        }
    }

    let created = current.canonicalize().map_err(|error| {
        OrbitError::Io(format!(
            "resolve granted write path `{}`: {error}",
            current.display()
        ))
    })?;
    if created != expected {
        return Err(OrbitError::InvalidInput(format!(
            "granted write directory `{}` resolved to `{}` after creation but was validated as \
             `{}`; a granted path cannot change where it points between the two",
            root.display(),
            created.display(),
            expected.display()
        )));
    }
    Ok(created)
}

/// Create one missing component, tolerating a concurrent creator only when
/// what appeared is a real directory.
fn create_directory_component(root: &Path, component: &Path) -> Result<(), OrbitError> {
    match orbit_common::fs::io::create_private_dir(component) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = std::fs::symlink_metadata(component).map_err(|error| {
                OrbitError::Io(format!(
                    "inspect concurrently created write directory `{}`: {error}",
                    component.display()
                ))
            })?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(OrbitError::InvalidInput(format!(
                    "granted write directory `{}` acquired an unsafe component `{}`",
                    root.display(),
                    component.display()
                )));
            }
            Ok(())
        }
        Err(error) => Err(OrbitError::Io(format!(
            "create granted write directory `{}`: {error}",
            component.display()
        ))),
    }
}
