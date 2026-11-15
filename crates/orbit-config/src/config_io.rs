//! Admitted, no-follow reads of `config.toml` files: the mechanism below
//! loading, diagnostics and `ConfigStore` mutation.
//!
//! This leaf holds only path admission and the read itself. Locking, staged
//! validation and atomic save stay with the store; policy, parsing and schema
//! stay with their owners.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::fs::open_read_only_no_follow;
use orbit_common::security::redaction::redact_home_dir;

/// Read an admitted config leaf, preserving the distinction between an absent
/// file and an empty document. Validate before opening, then check the opened
/// descriptor; the no-follow, nonblocking open also guards a replaced leaf.
pub(crate) fn read_config_file(path: &Path) -> Result<Option<String>, OrbitError> {
    let Some(path) = validated_config_store_path(path)? else {
        return Ok(None);
    };
    match open_read_only_no_follow(&path) {
        Ok(mut file) => {
            let metadata = file.metadata().map_err(|err| {
                OrbitError::Io(format!(
                    "failed to inspect config '{}': {err}",
                    redact_home_dir(&path.display().to_string())
                ))
            })?;
            if !metadata.is_file() {
                return Err(config_store_path_error(
                    "config path must be a regular file",
                    &path,
                ));
            }
            let mut content = String::new();
            file.read_to_string(&mut content).map_err(|err| {
                OrbitError::Io(format!(
                    "failed to read config '{}': {err}",
                    redact_home_dir(&path.display().to_string())
                ))
            })?;
            Ok(Some(content))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(OrbitError::Io(format!(
            "failed to read config '{}': {err}",
            redact_home_dir(&path.display().to_string())
        ))),
    }
}

/// CodeQL `rust/path-injection` treats `Path::starts_with` as a SafeAccessCheck
/// on the receiver. Call this after reconstructing a config path so later
/// filesystem sinks only see a prefix-checked value.
fn config_store_path_is_contained(path: &Path, parent: &Path) -> bool {
    path.starts_with(parent)
}

pub(crate) fn config_store_path_error(message: &str, path: &Path) -> OrbitError {
    OrbitError::InvalidInput(format!(
        "{message}: {}",
        redact_home_dir(&path.display().to_string())
    ))
}

/// Resolve a `config.toml` path before any read.
///
/// Callers pass a global or workspace config path. The parent is
/// canonicalized so aliases collapse, then the original file name is
/// rejoined and prefix-checked so later open/read sinks only see a
/// reconstructed path. A missing parent is treated as a missing file
/// (empty document), matching prior `read_optional` behavior. A present
/// leaf that is a symlink or non-file is refused.
pub(crate) fn validated_config_store_path(path: &Path) -> Result<Option<PathBuf>, OrbitError> {
    let Some(file_name) = path.file_name() else {
        return Err(config_store_path_error(
            "config path has no file name",
            path,
        ));
    };
    if file_name == "." || file_name == ".." {
        return Err(config_store_path_error(
            "config path must name a file",
            path,
        ));
    }
    let parent = path
        .parent()
        .ok_or_else(|| config_store_path_error("config path has no parent directory", path))?;
    let parent_for_resolution = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    let canonical_parent = match fs::canonicalize(parent_for_resolution) {
        Ok(parent) => parent,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(OrbitError::Io(format!(
                "failed to canonicalize config parent '{}': {error}",
                redact_home_dir(&parent_for_resolution.display().to_string())
            )));
        }
    };
    let candidate = canonical_parent.join(file_name);
    if !config_store_path_is_contained(&candidate, &canonical_parent) {
        return Err(config_store_path_error(
            "config path escapes its parent",
            path,
        ));
    }

    match fs::symlink_metadata(&candidate) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(config_store_path_error(
            "config path must not be a symlink",
            path,
        )),
        Ok(metadata) if !metadata.is_file() => Err(config_store_path_error(
            "config path must be a regular file",
            path,
        )),
        Ok(_) => Ok(Some(candidate)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Some(candidate)),
        Err(error) => Err(OrbitError::Io(format!(
            "failed to inspect config '{}': {error}",
            redact_home_dir(&candidate.display().to_string())
        ))),
    }
}
