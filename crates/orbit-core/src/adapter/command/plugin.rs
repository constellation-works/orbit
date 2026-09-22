//! Plugin lifecycle as the command surfaces reach it, plus the host-global
//! plugin tool surface the MCP server advertises.

use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_tools::plugin::stale_plugin_callback_session_count;
use orbit_types::tool::{McpToolDefinition, ToolSessionContext};
use serde_json::Value;

use crate::OrbitRuntime;
use crate::application::plugin;
use crate::runtime::plugin_host;

pub use crate::application::plugin::skills::PluginSkillLink;

pub use crate::application::plugin::{
    PluginAddOptions, PluginDoctorResult, PluginEnableOptions, PluginEnableResult,
    PluginLinkSummary, PluginMigrateRequest, PluginPanelSummary, PluginPermissionChange,
    PluginPermissionSummary, PluginRemoveOptions, PluginSeedAction, PluginSeedOutcome,
    PluginSummary, PluginSyncOutcome, PluginTestOptions, PluginTestOutcome, PluginTestReport,
    PluginToolSummary, PluginUpgradeOptions, PluginUpgradeResult, PluginValidationReport,
};
pub use crate::runtime::plugin_host::{PluginCliGroup, PluginCliVerb};

/// What `orbit plugin add --enable` produced, kept alongside the install
/// summary rather than collapsed into it, so the CLI can render seeded
/// schedules, linked skills and warnings the same way `orbit plugin enable`
/// does. `seeded`, `skills` and `warnings` are empty when `--enable` was not
/// set.
#[derive(Debug, Clone)]
pub struct PluginAddResult {
    pub summary: PluginSummary,
    pub seeded: Vec<PluginSeedOutcome>,
    pub skills: Vec<PluginSkillLink>,
    pub warnings: Vec<String>,
}

impl OrbitRuntime {
    pub fn add_plugin(
        &self,
        source: &str,
        options: &PluginAddOptions,
    ) -> Result<PluginAddResult, OrbitError> {
        let outcome = plugin::install_plugin_reporting_enable(self, source, options)?;
        Ok(PluginAddResult {
            summary: outcome.summary,
            seeded: outcome.seeded,
            skills: outcome.skills,
            warnings: outcome.warnings,
        })
    }

    pub fn upgrade_plugin(
        &self,
        name: &str,
        source: Option<&str>,
        options: &PluginUpgradeOptions,
    ) -> Result<PluginUpgradeResult, OrbitError> {
        plugin::upgrade_plugin(self, name, source, options)
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

    pub fn remove_plugin(
        &self,
        name: &str,
        options: &PluginRemoveOptions,
    ) -> Result<(), OrbitError> {
        plugin::remove_plugin(self, name, options)
    }

    pub fn list_plugins(&self) -> Result<Vec<PluginSummary>, OrbitError> {
        plugin::list_plugins(self)
    }

    /// Whether host plugin lifecycle state changed after this runtime built
    /// its plugin tool surface.
    ///
    /// Long-lived hosts use this cheap row comparison to discard the frozen
    /// runtime and rebuild it. The replacement then loads manifests, grants,
    /// tools, panels and links from one coherent pass.
    pub fn plugin_state_changed(&self) -> Result<bool, OrbitError> {
        Ok(self.stores().plugins().list_plugins()? != self.plugin_load().installed)
    }

    pub fn show_plugin(&self, name: &str) -> Result<PluginSummary, OrbitError> {
        plugin::show_plugin(self, name)
    }

    pub fn plugin_doctor(&self) -> Result<Vec<PluginDoctorResult>, OrbitError> {
        plugin::plugin_doctor(self)
    }

    /// Count callback records that no longer name a live plugin backend.
    pub fn stale_plugin_callback_session_count(&self) -> Result<usize, OrbitError> {
        stale_plugin_callback_session_count(&self.global_root())
    }

    /// Whether this host still accepts the retired plugin callback credential
    /// — the environment token plus process ancestry — alongside the
    /// descriptor a backend inherits. Reported by `orbit plugin doctor` for as
    /// long as it is on [ORB-12841].
    pub fn legacy_plugin_callback_identity(&self) -> Result<bool, OrbitError> {
        crate::adapter::command::dispatch::legacy_callback_identity_enabled(&self.global_root())
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

    pub fn sync_plugins(
        &self,
        dry_run: bool,
        grants: &[String],
    ) -> Result<Vec<PluginSyncOutcome>, OrbitError> {
        plugin::sync_plugins(self, dry_run, grants)
    }

    /// Run a plugin directory's `spec.tests` goldens through the real
    /// protocol in a temp workspace. `options` consents to an unconfined
    /// backend, an absolute write root, `network: any`, or `env_pass`;
    /// without that consent those requests are refused. A passing run records
    /// the Orbit version on this host's record when the directory is the
    /// installed one (§5).
    pub fn test_plugin_dir(
        &self,
        dir: &Path,
        options: &PluginTestOptions,
    ) -> Result<PluginTestReport, OrbitError> {
        plugin::test_plugin_dir(self, dir, options)
    }

    /// Execute one declared dashboard panel's `read_only` source (§4.7).
    pub fn read_plugin_panel(&self, namespace: &str, panel: &str) -> Result<Value, OrbitError> {
        plugin::read_plugin_panel(self, namespace, panel)
    }

    /// Effective server-side cache window for one dashboard panel.
    pub fn plugin_panel_refresh_ms(&self, namespace: &str, panel: &str) -> Result<u64, OrbitError> {
        plugin::plugin_panel_refresh_ms(self, namespace, panel)
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
    let config = global_only_config(global_root)?;
    plugin_host::host_plugin_cli_groups(global_root, &config.persistence.audit_db, &config.plugins)
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
    plugin_host::host_plugin_mcp_definitions(
        global_root,
        &config.persistence.audit_db,
        &config.plugins,
    )
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
    let config = global_only_config(global_root)?;
    let (registry, _load) = plugin_host::host_plugin_registry(
        global_root,
        &config.persistence.audit_db,
        &config.plugins,
    )?;
    super::dispatch::execute_global_plugin_dispatch(
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
fn global_only_config(global_root: &Path) -> Result<orbit_config::ResolvedConfig, OrbitError> {
    orbit_config::ResolvedConfig::load(&orbit_config::ConfigRoots::global_only(global_root))
}
