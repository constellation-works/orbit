//! Plugin lifecycle as the command surfaces reach it, plus the host-global
//! plugin tool surface the MCP server advertises.

use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_types::tool::{McpToolDefinition, ToolSessionContext};
use serde_json::Value;

use crate::OrbitRuntime;
use crate::application::plugin;
use crate::runtime::plugin_host;

pub use crate::application::plugin::{
    PluginAddOptions, PluginDoctorResult, PluginEnableOptions, PluginEnableResult,
    PluginLinkSummary, PluginMigrateRequest, PluginPanelSummary, PluginPermissionSummary,
    PluginSeedAction, PluginSeedOutcome, PluginSummary, PluginSyncOutcome, PluginTestOutcome,
    PluginTestReport, PluginToolSummary, PluginValidationReport,
};
pub use crate::runtime::plugin_host::{PluginCliGroup, PluginCliVerb};

impl OrbitRuntime {
    pub fn add_plugin(
        &self,
        source: &str,
        options: &PluginAddOptions,
    ) -> Result<PluginSummary, OrbitError> {
        plugin::install_plugin(self, source, options)
    }

    pub fn enable_plugin(
        &self,
        name: &str,
        options: &PluginEnableOptions,
    ) -> Result<PluginEnableResult, OrbitError> {
        plugin::enable_plugin(self, name, options)
    }

    pub fn disable_plugin(&self, name: &str) -> Result<PluginSummary, OrbitError> {
        plugin::disable_plugin(self, name)
    }

    pub fn remove_plugin(&self, name: &str) -> Result<(), OrbitError> {
        plugin::remove_plugin(self, name)
    }

    pub fn list_plugins(&self) -> Result<Vec<PluginSummary>, OrbitError> {
        plugin::list_plugins(self)
    }

    pub fn show_plugin(&self, name: &str) -> Result<PluginSummary, OrbitError> {
        plugin::show_plugin(self, name)
    }

    pub fn plugin_doctor(&self) -> Result<Vec<PluginDoctorResult>, OrbitError> {
        plugin::plugin_doctor(self)
    }

    /// Validate a plugin directory without installing it. `first_party` is
    /// the caller's statement that the source is a constellation-works
    /// checkout; it is only ever needed to validate an `origin: orbit`
    /// manifest.
    pub fn validate_plugin_dir(
        &self,
        dir: &Path,
        first_party: bool,
    ) -> Result<PluginValidationReport, OrbitError> {
        plugin::validate_plugin_dir(self, dir, first_party)
    }

    pub fn sync_plugins(&self, dry_run: bool) -> Result<Vec<PluginSyncOutcome>, OrbitError> {
        plugin::sync_plugins(self, dry_run)
    }

    /// Run a plugin directory's `spec.tests` goldens through the real
    /// protocol in a temp workspace, and record the passing Orbit version on
    /// this host's record when the directory is the installed one (§5).
    pub fn test_plugin_dir(&self, dir: &Path) -> Result<PluginTestReport, OrbitError> {
        plugin::test_plugin_dir(self, dir)
    }

    /// Execute one declared dashboard panel's `read_only` source (§4.7).
    pub fn read_plugin_panel(&self, namespace: &str, panel: &str) -> Result<Value, OrbitError> {
        plugin::read_plugin_panel(self, namespace, panel)
    }

    /// The `orbit <ns>` command groups this runtime's active plugins
    /// contribute. The CLI builds its clap tree before a runtime exists and
    /// uses [`host_plugin_cli_groups`]; this is the same projection for a
    /// caller that already holds one.
    pub fn plugin_cli_groups(&self) -> Vec<PluginCliGroup> {
        plugin_host::plugin_cli_groups(self.plugin_load())
    }
}

/// The `orbit <ns>` groups this host serves, read without a workspace
/// runtime (design §4.6).
pub fn host_plugin_cli_groups(global_root: &Path) -> Result<Vec<PluginCliGroup>, OrbitError> {
    plugin_host::host_plugin_cli_groups(global_root, &audit_db_path(global_root)?)
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
    plugin_host::host_plugin_mcp_definitions(global_root, &audit_db_path(global_root)?)
}

/// Execute a `mcp_scope: global` plugin tool without a workspace runtime,
/// inside Core's ordinary audited dispatch boundary and with the plugin's
/// provenance on the row.
pub fn execute_global_plugin_tool(
    global_root: &Path,
    name: &str,
    input: Value,
    entry_point: super::ToolEntryPoint,
    session_context: ToolSessionContext,
) -> Result<Value, OrbitError> {
    let (registry, _load) =
        plugin_host::host_plugin_registry(global_root, &audit_db_path(global_root)?)?;
    super::dispatch::execute_global_plugin_dispatch(
        global_root,
        name,
        input,
        entry_point,
        session_context,
        registry,
    )
}

fn audit_db_path(global_root: &Path) -> Result<PathBuf, OrbitError> {
    orbit_config::resolved_audit_db_path(&orbit_config::ConfigRoots::global_only(global_root))
}
