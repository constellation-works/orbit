//! Host-level plugin operations that run outside of a workspace runtime.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::plugin::InstalledPlugin;
use orbit_types::tool::{McpToolDefinition, ToolSessionContext};
use serde_json::Value;

use crate::adapter::command::ToolEntryPoint;
use crate::application::plugin::{global_only_config, global_only_config_with_env};
use crate::application::store_opener::StoreOpener;
use crate::runtime::plugin::discovery::{self, PluginCliGroup};

/// The `orbit <ns>` groups this host serves, read without a workspace
/// runtime (design §4.6).
pub fn host_plugin_cli_groups(
    opener: StoreOpener,
    global_root: &Path,
) -> Result<Vec<PluginCliGroup>, OrbitError> {
    let config = global_only_config(global_root)?;
    let store = opener.open_read_only(&config.persistence.audit_db)?;
    discovery::host_plugin_cli_groups(global_root, &store, &config.plugins)
}

/// MCP definitions for this host's active plugin tools.
///
/// Plugin installs are host-global, so this answers without a workspace and
/// is what the MCP server merges into `tools/list` alongside the canonical
/// built-in surface.
pub fn host_plugin_mcp_definitions(
    opener: StoreOpener,
    global_root: &Path,
) -> Result<Vec<McpToolDefinition>, OrbitError> {
    let config = global_only_config(global_root)?;
    let store = opener.open_read_only(&config.persistence.audit_db)?;
    discovery::host_plugin_mcp_definitions(global_root, &store, &config.plugins)
}

/// This host's plugin rows as stored now, for a long-lived host that keys a
/// cached runtime on them: a host enable or disable changes the rows, so the
/// cached runtime is rebuilt on the next call.
pub fn host_plugin_rows(
    opener: StoreOpener,
    global_root: &Path,
) -> Result<Vec<InstalledPlugin>, OrbitError> {
    let config = global_only_config(global_root)?;
    opener
        .open_read_only(&config.persistence.audit_db)?
        .read()
        .and_then(|snapshot| snapshot.plugin().list())
}

/// Execute a `mcp_scope: global` plugin tool without a workspace runtime,
/// inside Core's ordinary audited dispatch boundary and with the plugin's
/// provenance on the row.
pub fn execute_global_plugin_tool(
    env: &orbit_common::Env,
    opener: StoreOpener,
    global_root: &Path,
    name: &str,
    input: Value,
    entry_point: ToolEntryPoint,
    session_context: ToolSessionContext,
) -> Result<Value, OrbitError> {
    let config = global_only_config_with_env(env, global_root)?;
    let store = opener.open_read_only(&config.persistence.audit_db)?;
    let (registry, _load) = discovery::host_plugin_registry(global_root, &store, &config.plugins)?;
    drop(store);
    crate::adapter::command::dispatch::execute_global_plugin_dispatch(
        env,
        opener,
        global_root,
        name,
        input,
        entry_point,
        session_context,
        registry,
    )
}
