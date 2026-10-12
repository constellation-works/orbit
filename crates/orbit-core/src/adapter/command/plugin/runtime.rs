//! Runtime façade for plugin lifecycle, secrets, listing, doctor,
//! validation and panels.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_tools::plugin::stale_plugin_callback_session_count;
use serde_json::Value;

use crate::OrbitRuntime;
use crate::application::plugin::{
    self, PluginAddOptions, PluginDoctorResult, PluginEnableOptions, PluginEnableResult,
    PluginRemoveOptions, PluginSecretStatus, PluginSummary, PluginSyncOutcome, PluginTestOptions,
    PluginTestReport, PluginUpgradeOptions, PluginUpgradeResult, PluginValidationReport,
};
use crate::runtime::plugin::discovery::{self, PluginCliGroup};
use crate::runtime::plugin::secrets::PluginSecretValue;

use super::PluginAddResult;

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

    /// Re-enable from the dashboard only when grants and program paths still
    /// match the recorded operator decision.
    pub fn enable_plugin_from_dashboard(
        &self,
        name: &str,
    ) -> Result<PluginEnableResult, OrbitError> {
        plugin::enable_plugin_from_dashboard(self, name)
    }

    pub fn disable_plugin(&self, name: &str) -> Result<PluginSummary, OrbitError> {
        plugin::disable_plugin(self, name)
    }

    /// `orbit plugin enable <ns> --scope workspace`: the workspace toggle only.
    pub fn enable_plugin_in_workspace(
        &self,
        name: &str,
        force: bool,
    ) -> Result<PluginEnableResult, OrbitError> {
        plugin::enable_plugin_in_workspace(self, name, force)
    }

    /// `orbit plugin disable <ns> --scope workspace`: the workspace toggle only.
    pub fn disable_plugin_in_workspace(&self, name: &str) -> Result<PluginSummary, OrbitError> {
        plugin::disable_plugin_in_workspace(self, name)
    }

    pub fn remove_plugin(
        &self,
        name: &str,
        options: &PluginRemoveOptions,
    ) -> Result<(), OrbitError> {
        plugin::remove_plugin(self, name, options)
    }

    /// Store an operator-supplied value for one of the plugin's declared
    /// `spec.secrets`. The returned status carries no value.
    pub fn set_plugin_secret(
        &self,
        name: &str,
        secret: &str,
        value: &PluginSecretValue,
    ) -> Result<PluginSecretStatus, OrbitError> {
        plugin::set_plugin_secret(self, name, secret, value)
    }

    /// The plugin's secrets by name and set state, never their values.
    pub fn list_plugin_secrets(&self, name: &str) -> Result<Vec<PluginSecretStatus>, OrbitError> {
        plugin::list_plugin_secrets(self, name)
    }

    /// Delete one of the plugin's secrets; `Ok(false)` when it was not set.
    pub fn remove_plugin_secret(&self, name: &str, secret: &str) -> Result<bool, OrbitError> {
        plugin::remove_plugin_secret(self, name, secret)
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
        let load = self.plugin_load();
        if self.stores().plugins().list_plugins()? != load.installed {
            return Ok(true);
        }
        // A workspace toggle narrows the same surface, so flipping one in
        // `[plugin_enablement]` has to rebuild it too.
        Ok(plugin::workspace_plugin_toggles(self)? != load.workspace_toggles)
    }

    pub fn show_plugin(&self, name: &str) -> Result<PluginSummary, OrbitError> {
        plugin::show_plugin(self, name)
    }

    pub fn plugin_doctor(&self) -> Result<Vec<PluginDoctorResult>, OrbitError> {
        plugin::plugin_doctor(self)
    }

    /// The source-built plugin rows of [`Self::plugin_doctor`] alone.
    pub fn plugin_build_doctor(&self) -> Result<Vec<PluginDoctorResult>, OrbitError> {
        plugin::plugin_build_doctor(self)
    }

    /// Refuse, naming why, when this process runs inside an agent sandbox
    /// that masks plugin state and the plugin secret store.
    pub fn ensure_plugin_state_visible(&self) -> Result<(), OrbitError> {
        if crate::runtime::plugin::sandbox_mask::plugin_trees_masked(&self.global_root()) {
            return Err(crate::runtime::plugin::sandbox_mask::not_visible(
                "plugin state and secrets",
            ));
        }
        Ok(())
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

    /// Validate a plugin and render the profile/environment the backend would
    /// receive for `workspace` without executing it.
    pub fn validate_plugin_dir_rendered(
        &self,
        dir: &Path,
        first_party: bool,
        workspace: &Path,
    ) -> Result<PluginValidationReport, OrbitError> {
        plugin::validate_plugin_dir_for_workspace(self, dir, first_party, Some(workspace))
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
    /// uses [`super::host_plugin_cli_groups`]; this is the same projection for a
    /// caller that already holds one.
    pub fn plugin_cli_groups(&self) -> Vec<PluginCliGroup> {
        discovery::plugin_cli_groups(self.plugin_load())
    }
}
