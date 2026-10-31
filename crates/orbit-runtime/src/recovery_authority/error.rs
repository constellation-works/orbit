//! Error context shared by authority persistence and filesystem operations.

use orbit_common::OrbitError;
use std::path::Path;

pub(super) fn authority_error(action: &str, error: rusqlite::Error) -> OrbitError {
    OrbitError::Execution(format!("{action}: {error}"))
}

pub(super) fn path_error(action: &str, path: &Path, error: std::io::Error) -> OrbitError {
    OrbitError::Execution(format!("{action} `{}`: {error}", path.display()))
}
