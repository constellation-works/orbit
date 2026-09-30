//! Early check of an explicit Orbit root (`--root` / `ORBIT_ROOT`).
//!
//! The root is first touched by executable-generation pinning, whose failures
//! talk about admission and generations. When the operator simply pointed the
//! root at a file, or somewhere that can never become a directory, that is the
//! useful thing to say instead.

use std::path::Path;

use orbit_core::OrbitError;

/// Refuse a root that is not, and cannot become, a directory.
///
/// A missing root is allowed: first-create paths (`orbit init --root <new>`)
/// make it. Only a root that exists as a non-directory, or whose nearest
/// existing ancestor is not a directory, is rejected here. `source` names where
/// the root came from (`--root` or `ORBIT_ROOT`).
pub(crate) fn validate_explicit_root(root: &Path, source: &str) -> Result<(), OrbitError> {
    if root.exists() {
        return if root.is_dir() {
            Ok(())
        } else {
            Err(not_a_directory(root, source, "it is not a directory"))
        };
    }
    for ancestor in root.ancestors().skip(1) {
        if ancestor.as_os_str().is_empty() {
            break;
        }
        if ancestor.exists() {
            return if ancestor.is_dir() {
                Ok(())
            } else {
                Err(not_a_directory(
                    root,
                    source,
                    &format!("'{}' is not a directory", ancestor.display()),
                ))
            };
        }
    }
    Ok(())
}

/// Explain a failure to pin or create a root that did not exist beforehand.
pub(crate) fn missing_root_error(root: &Path, source: &str, cause: &OrbitError) -> OrbitError {
    not_a_directory(
        root,
        source,
        &format!("it does not exist and could not be created: {cause}"),
    )
}

fn not_a_directory(root: &Path, source: &str, why: &str) -> OrbitError {
    OrbitError::InvalidInput(format!(
        "{source} '{}' is not a usable Orbit root directory: {why}",
        root.display()
    ))
}
