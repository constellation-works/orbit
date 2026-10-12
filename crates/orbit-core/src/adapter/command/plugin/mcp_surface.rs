//! Stamp-aware host plugin discovery for long-lived MCP servers.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::SystemTime;

use orbit_common::OrbitError;
use orbit_config::ResolvedConfig;
use orbit_store::Store;
use orbit_types::plugin::{InstalledPlugin, MANIFEST_FILE_NAME, PluginStatus};
use orbit_types::tool::{McpToolDefinition, McpToolScope};

use crate::runtime::plugin::{build_witness, discovery, grants};

/// One coherent plugin surface, including workspace visibility ownership.
pub struct HostPluginMcpSurface {
    /// Active host plugin tool definitions.
    pub definitions: Vec<McpToolDefinition>,
    /// Workspace-scoped tools mapped to their serving plugin.
    pub workspace_tool_owners: BTreeMap<String, String>,
}

/// Reuses config, a read-only store handle and the manifest-derived surface.
/// Plugin rows are read at each boundary; unrelated audit writes do not
/// invalidate the surface. File stamps also cover config and admission files.
#[derive(Default)]
pub struct HostPluginMcpCache(Mutex<Option<CachedHost>>);

struct CachedHost {
    global_root: PathBuf,
    config_stamp: FileStamp,
    config: ResolvedConfig,
    database_identity: FileIdentity,
    store: Store,
    surface: Option<(PluginStamp, Arc<HostPluginMcpSurface>)>,
}

#[derive(PartialEq, Eq)]
struct PluginStamp {
    rows: Vec<InstalledPlugin>,
    files: Vec<FileStamp>,
}

#[derive(PartialEq, Eq)]
struct FileStamp {
    link: Option<(SystemTime, u64, bool)>,
    target: Option<(SystemTime, u64)>,
}

impl FileStamp {
    fn read(path: &Path) -> Self {
        Self {
            link: std::fs::symlink_metadata(path).ok().and_then(|metadata| {
                Some((
                    metadata.modified().ok()?,
                    metadata.len(),
                    metadata.file_type().is_symlink(),
                ))
            }),
            target: std::fs::metadata(path)
                .ok()
                .and_then(|metadata| Some((metadata.modified().ok()?, metadata.len()))),
        }
    }
}

/// Detect database replacement without treating every audit write as a change.
#[derive(PartialEq, Eq)]
struct FileIdentity {
    created: Option<SystemTime>,
    #[cfg(unix)]
    device_inode: (u64, u64),
}

impl FileIdentity {
    fn read(path: &Path) -> Result<Self, OrbitError> {
        let metadata = std::fs::metadata(path)?;
        Ok(Self {
            created: metadata.created().ok(),
            #[cfg(unix)]
            device_inode: {
                use std::os::unix::fs::MetadataExt;
                (metadata.dev(), metadata.ino())
            },
        })
    }
}

impl HostPluginMcpCache {
    /// Return the same snapshot while plugin admission inputs remain unchanged.
    /// Failed reads are never published as a reusable empty plugin surface.
    pub fn resolve(&self, global_root: &Path) -> Result<Arc<HostPluginMcpSurface>, OrbitError> {
        let mut cached = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let config_stamp = FileStamp::read(&global_root.join("config.toml"));
        if cached
            .as_ref()
            .is_none_or(|host| host.global_root != global_root || host.config_stamp != config_stamp)
        {
            // Drop the old state first: a failed config edit must not restore it.
            *cached = None;
            let config = super::global_only_config(global_root)?;
            let database_identity = FileIdentity::read(&config.persistence.audit_db)?;
            let store = Store::open_read_only(&config.persistence.audit_db)?;
            *cached = Some(CachedHost {
                global_root: global_root.to_path_buf(),
                config_stamp,
                config,
                database_identity,
                store,
                surface: None,
            });
        }
        let Some(host) = cached.as_mut() else {
            return Err(OrbitError::Store(
                "plugin discovery cache unavailable".into(),
            ));
        };
        let database_identity = FileIdentity::read(&host.config.persistence.audit_db)?;
        if host.database_identity != database_identity {
            host.surface = None;
            host.store = Store::open_read_only(&host.config.persistence.audit_db)?;
            host.database_identity = database_identity;
        }
        let rows = host.store.list_plugins()?;
        let files = rows
            .iter()
            .flat_map(|row| {
                [
                    FileStamp::read(&Path::new(&row.install_path).join(MANIFEST_FILE_NAME)),
                    FileStamp::read(&grants::plugin_grant_witness_path(global_root, &row.name)),
                    FileStamp::read(&build_witness::plugin_build_witness_path(
                        global_root,
                        &row.name,
                    )),
                ]
            })
            .collect();
        let stamp = PluginStamp { rows, files };
        if let Some((previous, surface)) = &host.surface
            && *previous == stamp
        {
            return Ok(Arc::clone(surface));
        }
        tracing::debug!(target: "orbit.mcp.plugin", "rebuilding plugin MCP surface");
        let (registry, load) = discovery::host_plugin_registry(
            global_root,
            &host.config.persistence.audit_db,
            &host.config.plugins,
        )?;
        let definitions = registry
            .mcp_tool_definitions()
            .map_err(|error| OrbitError::InvalidInput(error.to_string()))?;
        let owners = load
            .registered
            .iter()
            .filter(|entry| entry.status == PluginStatus::Active)
            .flat_map(|entry| {
                entry
                    .tools
                    .iter()
                    .map(|tool| (tool.as_str(), entry.name.as_str()))
            })
            .collect::<BTreeMap<_, _>>();
        let workspace_tool_owners = definitions
            .iter()
            .filter(|definition| definition.scope == McpToolScope::WorkspaceRequired)
            .filter_map(|definition| {
                let owner = owners.get(definition.schema.name.as_str())?;
                Some((definition.schema.name.clone(), (*owner).to_string()))
            })
            .collect();
        let surface = Arc::new(HostPluginMcpSurface {
            definitions,
            workspace_tool_owners,
        });
        // Discovery reads its own snapshot. A concurrent lifecycle write (or
        // a failed discovery row read) must not publish under the earlier stamp.
        if load.installed == stamp.rows {
            host.surface = Some((stamp, Arc::clone(&surface)));
        }
        Ok(surface)
    }
}
