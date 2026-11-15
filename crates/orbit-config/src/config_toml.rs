//! Shared read-and-parse of one admitted `config.toml`.
//!
//! Path admission and the no-follow read stay in `config_io`, which cannot
//! parse TOML. This leaf only turns those bytes into a document. Policy and
//! schema stay with their owners.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::security::redaction::redact_home_dir;

use crate::config_io::read_config_file;

/// Read an admitted config leaf and parse it as one TOML document.
///
/// A missing directory or file is `Ok(None)`. An empty file parses as an
/// empty document. A symlink or a non-regular leaf is refused, including a
/// leaf that is no longer a regular file after the no-follow open. A parse
/// failure names the home-redacted path: `invalid runtime config '<path>'`.
pub(crate) fn read_config_toml(path: &Path) -> Result<Option<toml::Value>, OrbitError> {
    let Some(raw) = read_config_file(path)? else {
        return Ok(None);
    };
    toml::from_str(&raw).map(Some).map_err(|error| {
        OrbitError::InvalidInput(format!(
            "invalid runtime config '{}': {error}",
            redact_home_dir(&path.display().to_string())
        ))
    })
}
