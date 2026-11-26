//! Process-local reuse of loaded plugin manifests and parsed definition sets
//! across host load passes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::SystemTime;

use orbit_tools::plugin::{
    LoadedPlugin, PluginLoadError, load_plugin_dir_without_tests, manifest_digest,
    resolve_plugin_definitions,
};
use orbit_types::plugin::{InstalledPlugin, MANIFEST_FILE_NAME};

use super::definitions::{
    PluginDefinitionSet, RecordedDefinitionRead, definition_file_stamp,
    load_plugin_definitions_recording, plugin_definition_paths, shipped_job_names,
};

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
/// manifest this process already resolved. Definition patterns are re-expanded
/// before reuse so file membership cannot remain pinned to the first load.
///
/// `definitions` is the parsed contribution set. It is reused only while the
/// row still matches and every definition file's stamp matches. The stamp
/// includes a content digest, not only length and mtime, so a same-length edit
/// or a permission change is parsed again.
#[derive(Clone)]
struct CachedPluginLoad {
    installed: InstalledPlugin,
    manifest_stamp: Option<PluginManifestStamp>,
    loaded: Arc<LoadedPlugin>,
    definitions: Option<CachedPluginDefinitions>,
}

/// Parsed definition files for one cached manifest.
#[derive(Clone)]
struct CachedPluginDefinitions {
    stamps: Vec<RecordedDefinitionRead>,
    parsed: Arc<PluginDefinitionSet>,
}

/// Process-local manifest and definition half of a host plugin load, shared by
/// pre-clap CLI discovery, runtime construction, and host-global MCP discovery.
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

pub fn load_installed_plugin(
    installed: &InstalledPlugin,
) -> Result<Arc<LoadedPlugin>, PluginLoadError> {
    let root = PathBuf::from(&installed.install_path);
    let manifest_stamp = PluginManifestStamp::read(&root);
    let cached = plugin_host_load_cache().plugins.get(&root).cloned();
    if let Some(cached) = cached.filter(|cached| cached.installed == *installed)
        && (cached.manifest_stamp == manifest_stamp
            || std::fs::read(root.join(MANIFEST_FILE_NAME))
                .ok()
                .is_some_and(|bytes| manifest_digest(&bytes) == cached.loaded.manifest_digest))
        && resolve_plugin_definitions(&cached.loaded.root, &cached.loaded.manifest)?
            == cached.loaded.definitions
    {
        if cached.manifest_stamp != manifest_stamp {
            plugin_host_load_cache().plugins.insert(
                root,
                CachedPluginLoad {
                    manifest_stamp,
                    ..cached.clone()
                },
            );
        }
        return Ok(cached.loaded);
    }

    // Host serving never reads `LoadedPlugin.tests`, so skip the golden
    // parse: no `spec.tests` file is opened on this path. Golden validation
    // still happens in `orbit plugin validate`/`test` and at install time,
    // which use the full loader.
    let loaded = Arc::new(load_plugin_dir_without_tests(&root)?);
    plugin_host_load_cache().plugins.insert(
        root,
        CachedPluginLoad {
            installed: installed.clone(),
            manifest_stamp,
            loaded: Arc::clone(&loaded),
            definitions: None,
        },
    );
    Ok(loaded)
}

/// Parse a plugin's definition files, or reuse the set this process already
/// parsed for the same row.
///
/// A reuse requires the installed row, the loaded manifest's definition paths,
/// and every definition file's stamp to be unchanged. The stamp check reads
/// the files; it does not parse them. A parse failure drops any set stored for
/// this row so the next pass cannot keep a plugin active on stale bytes.
pub(crate) fn cached_plugin_definitions(
    installed: &InstalledPlugin,
    plugin: &LoadedPlugin,
) -> Result<Arc<PluginDefinitionSet>, String> {
    let root = PathBuf::from(&installed.install_path);
    let cached = {
        let cache = plugin_host_load_cache();
        cache.plugins.get(&root).and_then(|entry| {
            if entry.installed == *installed && same_loaded_plugin(&entry.loaded, plugin) {
                entry.definitions.clone()
            } else {
                None
            }
        })
    };
    if let Some(cached) = cached
        && definition_stamps(plugin).is_some_and(|fresh| fresh == cached.stamps)
    {
        return Ok(cached.parsed);
    }

    let mut stamps = Vec::new();
    let parsed = load_plugin_definitions_recording(plugin, &shipped_job_names(), Some(&mut stamps))
        .map(Arc::new);
    let mut cache = plugin_host_load_cache();
    if let Some(entry) = cache.plugins.get_mut(&root)
        && entry.installed == *installed
        && same_loaded_plugin(&entry.loaded, plugin)
    {
        entry.definitions = match &parsed {
            Ok(set) if stamps_cover_plugin(plugin, &stamps) => Some(CachedPluginDefinitions {
                stamps,
                parsed: Arc::clone(set),
            }),
            _ => None,
        };
    }
    parsed
}

fn same_loaded_plugin(cached: &LoadedPlugin, plugin: &LoadedPlugin) -> bool {
    cached.root == plugin.root
        && cached.manifest_digest == plugin.manifest_digest
        && cached.definitions == plugin.definitions
}

fn definition_stamps(plugin: &LoadedPlugin) -> Option<Vec<RecordedDefinitionRead>> {
    let mut stamps = Vec::new();
    for path in plugin_definition_paths(plugin) {
        stamps.push(definition_file_stamp(path)?);
    }
    Some(stamps)
}

fn stamps_cover_plugin(plugin: &LoadedPlugin, stamps: &[RecordedDefinitionRead]) -> bool {
    let paths: Vec<&Path> = plugin_definition_paths(plugin).collect();
    stamps.len() == paths.len()
        && stamps
            .iter()
            .zip(paths)
            .all(|(stamp, path)| stamp.path == path)
}
