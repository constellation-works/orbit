use std::time::Duration;

use orbit_common::OrbitError;
use orbit_common::governance::authorization::{DASHBOARD_PLUGIN_DISABLE, DASHBOARD_PLUGIN_ENABLE};
use serde_json::Value;

use super::Outcome;
use crate::app::exec;
use crate::{Core, OrbitRuntime};

/// The enablement record a dashboard write changes.
pub enum DashboardScope {
    Host,
    Workspace,
}

/// One enable or disable request from an authenticated dashboard session.
pub struct DashboardToggle {
    pub namespace: String,
    pub scope: Result<DashboardScope, OrbitError>,
    pub enable: bool,
    pub operator_session: bool,
    pub arguments: Value,
    pub host: Option<String>,
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

pub(super) fn exec(core: &Core, toggle: DashboardToggle) -> Result<Outcome, OrbitError> {
    let DashboardToggle {
        namespace,
        scope,
        enable,
        operator_session,
        arguments,
        host,
    } = toggle;
    let operation = if enable {
        &DASHBOARD_PLUGIN_ENABLE
    } else {
        &DASHBOARD_PLUGIN_DISABLE
    };
    let runtime = core.runtime();
    exec::run_dashboard(
        core,
        exec::DashboardOperation {
            operation,
            target: &namespace,
            arguments,
            operator_session,
            host,
        },
        || {
            let scope = scope?;
            if !runtime
                .list_plugins()?
                .iter()
                .any(|plugin| plugin.name == namespace)
            {
                return Ok(None);
            }
            let summary = match (enable, scope) {
                (true, DashboardScope::Host) => {
                    runtime.enable_plugin_from_dashboard(&namespace)?.summary
                }
                (true, DashboardScope::Workspace) => {
                    runtime
                        .enable_plugin_in_workspace(&namespace, false)?
                        .summary
                }
                (false, DashboardScope::Host) => runtime.disable_plugin(&namespace)?,
                (false, DashboardScope::Workspace) => {
                    runtime.disable_plugin_in_workspace(&namespace)?
                }
            };
            Ok(Some(summary))
        },
        |summary| {
            summary
                .is_none()
                .then(|| format!("plugin not found: {namespace}"))
        },
    )
    .map(Outcome::DashboardToggled)
}
