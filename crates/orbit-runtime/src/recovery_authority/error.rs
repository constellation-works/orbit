//! Error context for authority filesystem operations.

use std::path::Path;

use orbit_common::OrbitError;

pub(super) fn path_error(action: &str, path: &Path, error: std::io::Error) -> OrbitError {
    OrbitError::Execution(format!("{action} `{}`: {error}", path.display()))
}
