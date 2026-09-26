//! Where plugins live on this host, and the workspace's committed pin file.

use std::io::Read;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_types::plugin::PluginPinFile;

/// Where a plugin lives on this host: `<global>/plugins/<ns>/<version>`.
pub fn plugin_install_root(global_root: &Path) -> PathBuf {
    global_root.join("plugins")
}

/// The one directory this host installs every version of `name` into.
///
/// Trusted layout: it is derived from the namespace and the global root, never
/// from the `plugins` row, so a lifecycle verb can clean up after a row whose
/// recorded `install_path` it refuses to touch [ORB-12800].
pub fn plugin_namespace_dir(global_root: &Path, name: &str) -> PathBuf {
    plugin_install_root(global_root).join(name)
}

pub fn plugin_install_path(global_root: &Path, name: &str, version: &str) -> PathBuf {
    plugin_namespace_dir(global_root, name).join(version)
}

/// Per-plugin state directory handed to the backend as `ORBIT_PLUGIN_STATE`.
pub fn plugin_state_dir(global_root: &Path, name: &str) -> PathBuf {
    global_root.join("state").join("plugins").join(name)
}

/// The host-owned secret store (`runtime::plugin::secrets`). No plugin child
/// can read it: it is on the plugin sandbox's unreadable list.
pub fn plugin_secret_store_dir(global_root: &Path) -> PathBuf {
    global_root.join(orbit_tools::plugin::PLUGIN_SECRET_STORE_DIR)
}

/// The workspace's committed pin file, when it has one.
pub fn read_pin_file(orbit_dir: &Path) -> Result<Option<PluginPinFile>, OrbitError> {
    let Ok(path) = validated_pin_file_path(orbit_dir) else {
        return Ok(None);
    };
    let Ok(mut file) = orbit_common::fs::io::open_read_only_no_follow(&path) else {
        return Ok(None);
    };
    let Ok(metadata) = file.metadata() else {
        return Ok(None);
    };
    if !metadata.is_file() {
        return Ok(None);
    }
    let mut raw = String::new();
    if file.read_to_string(&mut raw).is_err() {
        return Ok(None);
    }
    let pins: PluginPinFile = serde_yaml::from_str(&raw).map_err(|error| {
        OrbitError::InvalidInput(format!("invalid {}: {error}", path.display()))
    })?;
    pins.validate()
        .map_err(|error| OrbitError::InvalidInput(format!("{}: {error}", path.display())))?;
    Ok(Some(pins))
}

/// Resolve the runtime-selected `.orbit` directory before appending the fixed
/// pin filename. The leaf is opened with no-follow semantics by the caller.
fn validated_pin_file_path(orbit_dir: &Path) -> std::io::Result<PathBuf> {
    let root = crate::runtime::config_path::validated_existing_config_root(orbit_dir)
        .map_err(|error| std::io::Error::other(error.to_string()))?
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "plugin pin root does not exist",
            )
        })?;
    if !std::fs::metadata(&root)?.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotADirectory,
            "plugin pin root is not a directory",
        ));
    }
    Ok(root.join(orbit_types::plugin::PIN_FILE_NAME))
}
