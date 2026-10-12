//! Host-level plugin operations that run outside of a workspace runtime.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_types::plugin::{InstalledPlugin, PluginStatus};
use orbit_types::tool::{McpToolDefinition, McpToolScope, ToolSessionContext};
use serde_json::Value;

use crate::adapter::command::ToolEntryPoint;
use crate::application::plugin::{self, PluginMigrateRequest};
use crate::runtime::plugin::discovery::{self, PluginCliGroup};

/// The `orbit <ns>` groups this host serves, read without a workspace
/// runtime (design §4.6).
pub fn host_plugin_cli_groups(global_root: &Path) -> Result<Vec<PluginCliGroup>, OrbitError> {
    let config = global_only_config(global_root)?;
    discovery::host_plugin_cli_groups(global_root, &config.persistence.audit_db, &config.plugins)
}

/// Write a v2 manifest from a set of v1 `*.orbit-tool.yaml` sidecars. No
/// runtime is involved: this reads files and writes one.
pub fn migrate_plugin_sidecars(
    request: &PluginMigrateRequest,
) -> Result<(String, Option<PathBuf>), OrbitError> {
    plugin::migrate_plugin_sidecars(request)
}

/// MCP definitions for this host's active plugin tools.
///
/// Plugin installs are host-global, so this answers without a workspace and
/// is what the MCP server merges into `tools/list` alongside the canonical
/// built-in surface.
pub fn host_plugin_mcp_definitions(
    global_root: &Path,
) -> Result<Vec<McpToolDefinition>, OrbitError> {
    let config = global_only_config(global_root)?;
    discovery::host_plugin_mcp_definitions(
        global_root,
        &config.persistence.audit_db,
        &config.plugins,
    )
}

/// This host's plugin rows as stored now, for a long-lived host that keys a
/// cached runtime on them: a host enable or disable changes the rows, so the
/// cached runtime is rebuilt on the next call.
pub fn host_plugin_rows(global_root: &Path) -> Result<Vec<InstalledPlugin>, OrbitError> {
    let config = global_only_config(global_root)?;
    orbit_store::Store::open_read_only(&config.persistence.audit_db)?.list_plugins()
}

/// This host's workspace-scoped plugin tools (`mcp_scope: workspace`), each
/// mapped to the plugin that serves it.
///
/// Only these can be switched off per workspace: a `mcp_scope: global` tool
/// has no workspace and follows the host state alone. The MCP server uses the
/// map to leave a workspace-disabled plugin's tools out of `tools/list`.
pub fn host_plugin_workspace_tool_owners(
    global_root: &Path,
) -> Result<BTreeMap<String, String>, OrbitError> {
    let config = global_only_config(global_root)?;
    let (registry, load) = discovery::host_plugin_registry(
        global_root,
        &config.persistence.audit_db,
        &config.plugins,
    )?;
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
    let definitions = registry
        .mcp_tool_definitions()
        .map_err(|error| OrbitError::InvalidInput(error.to_string()))?;
    Ok(definitions
        .into_iter()
        .filter(|definition| definition.scope == McpToolScope::WorkspaceRequired)
        .filter_map(|definition| {
            let owner = owners.get(definition.schema.name.as_str())?;
            Some((definition.schema.name, (*owner).to_string()))
        })
        .collect())
}

/// Execute a `mcp_scope: global` plugin tool without a workspace runtime,
/// inside Core's ordinary audited dispatch boundary and with the plugin's
/// provenance on the row.
pub fn execute_global_plugin_tool(
    global_root: &Path,
    name: &str,
    input: Value,
    entry_point: ToolEntryPoint,
    session_context: ToolSessionContext,
) -> Result<Value, OrbitError> {
    let config = global_only_config(global_root)?;
    let (registry, _load) = discovery::host_plugin_registry(
        global_root,
        &config.persistence.audit_db,
        &config.plugins,
    )?;
    crate::adapter::command::dispatch::execute_global_plugin_dispatch(
        global_root,
        name,
        input,
        entry_point,
        session_context,
        registry,
    )
}

/// This host's config with no workspace layered over it: `~/.orbit/config.toml`
/// alone, read via `ConfigRoots::global_only`. Every host-global plugin
/// surface (CLI groups, MCP `tools/list`, global tool execution) resolves its
/// `[plugins.<ns>]` sections and audit db path from this, never a bare
/// `BTreeMap::new()`, so a schema-required key set only in `config.toml`
/// still lets the plugin load here exactly as it does under a workspace
/// runtime (design §1, §4.7).
pub(crate) fn global_only_config(
    global_root: &Path,
) -> Result<orbit_config::ResolvedConfig, OrbitError> {
    orbit_config::ResolvedConfig::load(&orbit_config::ConfigRoots::global_only(global_root))
}
