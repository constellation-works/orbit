//! `orbit plugin add`: resolve a source, refuse an in-repository one, copy the
//! tree into the host install root, and record it.

mod flow;
mod permissions;
mod staging;

pub(super) use flow::install_pinned_plugin;
pub(crate) use flow::install_plugin_reporting_enable;
pub use flow::{install_plugin, upgrade_plugin};
pub(super) use staging::lock_plugin_namespace;

use orbit_types::plugin::PluginGrant;

use super::inspect::PluginSummary;
use super::seed::PluginSeedOutcome;
use super::skills::PluginSkillLink;

#[derive(Debug, Clone, Default)]
pub struct PluginAddOptions {
    /// Replace an existing install of the same namespace and version.
    pub force: bool,
    /// `sha256:<hex>` an `https://` archive source must hash to. Such a
    /// source is refused without one; every other source ignores it.
    pub digest: Option<String>,
    /// Enable the plugin as part of the install.
    pub enable: bool,
    /// Grants recorded when `enable` is set.
    pub grants: Vec<String>,
    /// The operator's `--allow-build`: consent to run this source's
    /// `spec.build` (design `docs/design/plugins/3_install_time_build.md`
    /// §3.8). Never set from a pin, config or any unattended caller.
    pub allow_build: bool,
    /// Shows the build plan before a consented build runs.
    pub show_build_plan: Option<fn(&str)>,
}

/// One requested-permission change between the installed and candidate
/// manifests. `widened` means carrying the old grant would authorize
/// something the operator did not previously review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginPermissionChange {
    pub grant: PluginGrant,
    pub previous: Option<String>,
    pub requested: Option<String>,
    pub widened: bool,
}

#[derive(Debug, Clone, Default)]
pub struct PluginUpgradeOptions {
    /// `sha256:<hex>` for an `https://` archive source, as in
    /// [`PluginAddOptions::digest`]. An archive source is re-fetched
    /// on upgrade, so the new archive needs its own pin.
    pub digest: Option<String>,
    /// Complete grant set authorizing and enabling the upgraded manifest.
    /// Without it, a safe upgrade preserves the existing row; a widening
    /// disables the plugin and clears its grants.
    pub grants: Vec<String>,
    /// As [`PluginAddOptions::allow_build`]: consent never carries over from
    /// the previous install.
    pub allow_build: bool,
    /// As [`PluginAddOptions::show_build_plan`].
    pub show_build_plan: Option<fn(&str)>,
}

#[derive(Debug, Clone)]
pub struct PluginUpgradeResult {
    pub summary: PluginSummary,
    pub permission_changes: Vec<PluginPermissionChange>,
    pub grants_reset: bool,
}

pub(crate) struct PluginInstallOutcome {
    pub(crate) summary: PluginSummary,
    permission_changes: Vec<PluginPermissionChange>,
    grants_reset: bool,
    /// Routines and auto-tasks seeded by `--enable`; empty otherwise.
    pub(crate) seeded: Vec<PluginSeedOutcome>,
    /// Skill links maintained by `--enable`; empty otherwise.
    pub(crate) skills: Vec<PluginSkillLink>,
    /// Non-fatal problems `--enable` surfaced; empty otherwise.
    pub(crate) warnings: Vec<String>,
}
