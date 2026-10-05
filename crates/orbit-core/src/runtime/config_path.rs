//! Selection of the effective `config.toml` from a runtime's roots.

use std::path::{Path, PathBuf};

use orbit_common::OrbitError;

use super::OrbitRuntime;

/// Select the fixed config leaf from roots owned by an initialized runtime.
///
/// Keeping this as a free-function boundary gives static analysis a precise
/// callable whose return is authoritative. It does not authorize an arbitrary
/// caller-supplied root: both roots come from the runtime's resolved context.
pub(super) fn validated_runtime_config_path(runtime: &OrbitRuntime) -> Result<PathBuf, OrbitError> {
    let shared_root = runtime.shared_root();
    let global_root = runtime.global_root();
    if shared_root != global_root
        && let Some(workspace_config) = existing_config_file_path(&shared_root)?
    {
        return Ok(workspace_config);
    }

    Ok(global_root.join(CONFIG_TOML_FILE))
}

pub(crate) const CONFIG_TOML_FILE: &str = "config.toml";

/// Resolve an existing config root to the directory selected by the caller.
///
/// Runtime overrides and trusted directory aliases remain supported: an
/// existing symlinked root resolves to its canonical target. Returning that
/// validated root before deriving the fixed filename puts the validation
/// boundary ahead of the child metadata probe. Final-component safety belongs
/// to the descriptor-based reader; this boundary does not claim protection
/// against privileged replacement of mutable ancestors.
pub(super) fn validated_existing_config_root(root: &Path) -> Result<Option<PathBuf>, OrbitError> {
    let canonical_root = match root.canonicalize() {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(OrbitError::Io(format!(
                "failed to canonicalize config root '{}': {error}",
                root.display()
            )));
        }
    };

    Ok(Some(canonical_root))
}

/// Select the fixed config leaf of one layer root after the root validation
/// boundary. `None` means the root or the file is absent.
///
/// This no-follow metadata probe decides precedence and rejects an already
/// visible invalid leaf. It does not authorize a later pathname read; config
/// consumers reopen the selected path through their descriptor-based boundary.
/// Other modules that need to know whether a layer file exists probe through
/// this function rather than `Path::exists` so the probe only ever sees a
/// validated root [ORB-12724].
pub(crate) fn existing_config_file_path(root: &Path) -> Result<Option<PathBuf>, OrbitError> {
    let Some(validated_root) = validated_existing_config_root(root)? else {
        return Ok(None);
    };
    let candidate = validated_root.join(CONFIG_TOML_FILE);

    match std::fs::symlink_metadata(&candidate) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(OrbitError::InvalidInput(format!(
                "config path must be a regular {CONFIG_TOML_FILE} file inside '{}': {}",
                root.display(),
                candidate.display()
            )))
        }
        Ok(_) => Ok(Some(candidate)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(OrbitError::Io(format!(
            "failed to inspect config path '{}': {error}",
            candidate.display()
        ))),
    }
}
