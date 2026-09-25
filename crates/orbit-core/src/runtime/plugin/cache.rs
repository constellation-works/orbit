//! Process-local reuse of loaded plugin manifests across host load passes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::SystemTime;

use orbit_tools::plugin::{LoadedPlugin, PluginLoadError, load_plugin_dir, manifest_digest};
use orbit_types::plugin::{InstalledPlugin, MANIFEST_FILE_NAME};

/// The cheap facts used to decide whether a previously loaded manifest can be
/// reused without walking its whole install tree again.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PluginManifestStamp {
    modified: Option<SystemTime>,
    len: u64,
    is_file: bool,
    is_symlink: bool,
}

impl PluginManifestStamp {
    fn read(plugin_root: &Path) -> Option<Self> {
        let metadata = std::fs::symlink_metadata(plugin_root.join(MANIFEST_FILE_NAME)).ok()?;
        Some(Self {
            modified: metadata.modified().ok(),
            len: metadata.len(),
            is_file: metadata.is_file(),
            is_symlink: metadata.file_type().is_symlink(),
        })
    }
}

/// One reusable part of a host load.
///
/// The exact row is part of the key because enable state, grants, install
/// path, and recorded digest all affect admission. The manifest stamp is the
/// common fast path. When only that stamp changes, the bytes are hashed before
/// the expensive symlink walk: an unchanged digest still names the same
/// manifest this process already resolved.
#[derive(Clone)]
struct CachedPluginLoad {
    installed: InstalledPlugin,
    manifest_stamp: Option<PluginManifestStamp>,
    loaded: Arc<LoadedPlugin>,
}

/// Process-local manifest half of [`PluginHostLoad`](super::host::PluginHostLoad),
/// shared by pre-clap CLI discovery, runtime construction, and host-global MCP
/// discovery.
#[derive(Default)]
struct PluginHostLoadCache {
    plugins: BTreeMap<PathBuf, CachedPluginLoad>,
}

static PLUGIN_HOST_LOAD_CACHE: OnceLock<Mutex<PluginHostLoadCache>> = OnceLock::new();

fn plugin_host_load_cache() -> MutexGuard<'static, PluginHostLoadCache> {
    PLUGIN_HOST_LOAD_CACHE
        .get_or_init(|| Mutex::new(PluginHostLoadCache::default()))
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

pub(crate) fn load_installed_plugin(
    installed: &InstalledPlugin,
) -> Result<Arc<LoadedPlugin>, PluginLoadError> {
    let root = PathBuf::from(&installed.install_path);
    let manifest_stamp = PluginManifestStamp::read(&root);
    let cached = plugin_host_load_cache().plugins.get(&root).cloned();
    if let Some(cached) = cached.filter(|cached| cached.installed == *installed) {
        if cached.manifest_stamp == manifest_stamp {
            return Ok(cached.loaded);
        }
        if std::fs::read(root.join(MANIFEST_FILE_NAME))
            .ok()
            .is_some_and(|bytes| manifest_digest(&bytes) == cached.loaded.manifest_digest)
        {
            plugin_host_load_cache().plugins.insert(
                root,
                CachedPluginLoad {
                    manifest_stamp,
                    ..cached.clone()
                },
            );
            return Ok(cached.loaded);
        }
    }

    #[cfg(test)]
    record_plugin_dir_load(&root);
    let loaded = Arc::new(load_plugin_dir(&root)?);
    plugin_host_load_cache().plugins.insert(
        root,
        CachedPluginLoad {
            installed: installed.clone(),
            manifest_stamp,
            loaded: Arc::clone(&loaded),
        },
    );
    Ok(loaded)
}

#[cfg(test)]
static PLUGIN_DIR_LOAD_COUNTS: OnceLock<Mutex<BTreeMap<PathBuf, usize>>> = OnceLock::new();

#[cfg(test)]
fn record_plugin_dir_load(root: &Path) {
    let counts = PLUGIN_DIR_LOAD_COUNTS.get_or_init(|| Mutex::new(BTreeMap::new()));
    let mut counts = counts.lock().unwrap_or_else(PoisonError::into_inner);
    *counts.entry(root.to_path_buf()).or_default() += 1;
}

#[cfg(test)]
pub(super) fn plugin_dir_load_count(root: &Path) -> usize {
    PLUGIN_DIR_LOAD_COUNTS
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(root)
        .copied()
        .unwrap_or_default()
}
