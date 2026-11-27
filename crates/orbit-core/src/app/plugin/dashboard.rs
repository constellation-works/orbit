use std::time::Duration;

use orbit_common::OrbitError;
use orbit_common::governance::authorization::{DASHBOARD_PLUGIN_DISABLE, DASHBOARD_PLUGIN_ENABLE};
use serde_json::Value;

use super::Outcome;
use crate::app::exec::{DashboardOrigin, DashboardRow, Operation};
use crate::application::plugin::PluginSummary;
use crate::{Core, OrbitRuntime};

/// The enablement record a dashboard write changes.
pub enum DashboardScope {
    Host,
    Workspace,
}

/// One enable or disable request from a dashboard session.
pub struct DashboardToggle {
    pub namespace: String,
    pub scope: Result<DashboardScope, OrbitError>,
    pub enable: bool,
    pub arguments: Value,
    pub host: Option<String>,
}

impl DashboardToggle {
    /// The governed operation this toggle requires and its audit row names.
    pub(super) fn operation(&self) -> Operation {
        let governed = if self.enable {
            &DASHBOARD_PLUGIN_ENABLE
        } else {
            &DASHBOARD_PLUGIN_DISABLE
        };
        Operation::dashboard_read("plugin", "dashboard-toggle")
            .requires(governed)
            .dashboard_row(
                governed,
                self.namespace.clone(),
                self.arguments.clone(),
                DashboardOrigin {
                    host: self.host.clone(),
                    ..Default::default()
                },
            )
    }
}

/// A panel read bound to the loaded plugin and its cache window. The dashboard
/// executes this read on a cache miss, so a cache hit never invokes the tool.
pub struct PanelRead {
    runtime: OrbitRuntime,
    namespace: String,
    panel: String,
    ttl: Duration,
}

impl PanelRead {
    pub(super) fn new(core: &Core, namespace: String, panel: String) -> Result<Self, OrbitError> {
        let runtime = core.runtime();
        let refresh_ms = runtime.plugin_panel_refresh_ms(&namespace, &panel)?;
        Ok(Self {
            runtime: runtime.clone(),
            namespace,
            panel,
            ttl: Duration::from_millis(refresh_ms),
        })
    }

    /// How long a cached result for this panel remains fresh.
    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    /// Run the panel source through the existing read-only tool dispatch.
    pub fn read(self) -> Result<Value, OrbitError> {
        self.runtime.read_plugin_panel(&self.namespace, &self.panel)
    }
}

/// Change the enablement record, or answer `None` for an unknown plugin.
pub(super) fn toggle(
    core: &Core,
    toggle: DashboardToggle,
) -> Result<Option<PluginSummary>, OrbitError> {
    let runtime = core.runtime();
    let DashboardToggle {
        namespace,
        scope,
        enable,
        ..
    } = toggle;
    let scope = scope?;
    if !runtime
        .list_plugins()?
        .iter()
        .any(|plugin| plugin.name == namespace)
    {
        return Ok(None);
    }
    let summary = match (enable, scope) {
        (true, DashboardScope::Host) => runtime.enable_plugin_from_dashboard(&namespace)?.summary,
        (true, DashboardScope::Workspace) => {
            runtime
                .enable_plugin_in_workspace(&namespace, false)?
                .summary
        }
        (false, DashboardScope::Host) => runtime.disable_plugin(&namespace)?,
        (false, DashboardScope::Workspace) => runtime.disable_plugin_in_workspace(&namespace)?,
    };
    Ok(Some(summary))
}

/// A toggle's audit row: a missing plugin carries the same message as the
/// dashboard response.
pub(super) fn row(
    result: &Result<Outcome, OrbitError>,
    target: &str,
    arguments: Value,
) -> DashboardRow {
    match result {
        Ok(Outcome::DashboardToggled(None)) => DashboardRow {
            failure: Some(format!("plugin not found: {target}")),
            target: None,
            arguments,
        },
        result => DashboardRow::of(result, arguments),
    }
}
