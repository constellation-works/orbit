//! Whether a definition a plugin seeded is live where it lives.
//!
//! A seeded routine or auto-task stays on disk when its plugin is switched
//! off, edits and all, but it never fires. Execution skips it and every list
//! surface (CLI, MCP, dashboard) hides it by default, listing it again only
//! when the caller opts in. Both decisions come from [`inactive_plugin`], so
//! a definition is never shown as live while the scheduler skips it.

use std::path::Path;

use serde::Serialize;

use crate::runtime::plugin::host::PluginHostLoad;

/// Where the plugin that seeded a definition is switched off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InactivePluginScope {
    /// Enabled on the host, switched off by this workspace's toggle.
    Workspace,
    /// Disabled, removed or refused on the host.
    Host,
}

/// The plugin a definition's provenance names, and why it is not active.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InactivePlugin {
    pub namespace: String,
    pub version: String,
    pub scope: InactivePluginScope,
}

/// One workspace's view of which plugins are on.
pub trait PluginActivity {
    /// Whether the plugin's definitions are on the active surface here.
    fn is_active(&self, namespace: &str) -> bool;
    /// Whether this workspace's toggle switched off a host-enabled plugin.
    fn is_disabled_in_workspace(&self, namespace: &str) -> bool;
}

impl PluginActivity for PluginHostLoad {
    fn is_active(&self, namespace: &str) -> bool {
        PluginHostLoad::is_active(self, namespace)
    }

    fn is_disabled_in_workspace(&self, namespace: &str) -> bool {
        PluginHostLoad::is_disabled_in_workspace(self, namespace)
    }
}

/// The plugin that makes the definition at `path` inactive, if any.
///
/// `definitions_dir` is the routines or auto-tasks directory that may contain
/// `path`. A path that leaves that directory has no provenance. A definition
/// whose header names a plugin that is not active in `activity` is inactive:
/// the scheduler skips it and listings hide it unless asked. A user-authored
/// definition (no header) and one seeded by an active plugin return `None`.
pub fn inactive_plugin(
    definitions_dir: &Path,
    path: &Path,
    activity: &dyn PluginActivity,
) -> Option<InactivePlugin> {
    let (namespace, version) = super::read_definition_provenance(definitions_dir, path)?;
    if activity.is_active(&namespace) {
        return None;
    }
    let scope = if activity.is_disabled_in_workspace(&namespace) {
        InactivePluginScope::Workspace
    } else {
        InactivePluginScope::Host
    };
    Some(InactivePlugin {
        namespace,
        version,
        scope,
    })
}

/// Whether a list surface shows a definition: always when its plugin is
/// active (or it has none), otherwise only when the caller opted in with
/// `include_inactive_plugins`.
pub fn is_listed(plugin_inactive: bool, include_inactive_plugins: bool) -> bool {
    !plugin_inactive || include_inactive_plugins
}

impl InactivePlugin {
    /// The operator-facing skip reason: which plugin, the command that turns
    /// it back on, and the file to delete instead.
    ///
    /// `workspace` names the workspace for a host-wide surface (routines span
    /// every registered workspace); `None` reads as "this workspace".
    pub fn reason(&self, path: &Path, workspace: Option<&str>) -> String {
        let Self {
            namespace, version, ..
        } = self;
        match (self.scope, workspace) {
            (InactivePluginScope::Workspace, None) => format!(
                "seeded by plugin:{namespace}@{version}, which is switched off in this \
                 workspace; run `orbit plugin enable {namespace} --scope workspace` to fire it \
                 again, or delete '{}'",
                path.display()
            ),
            (InactivePluginScope::Workspace, Some(workspace)) => format!(
                "seeded by plugin:{namespace}@{version}, which is switched off in workspace \
                 '{workspace}'; run `orbit plugin enable {namespace} --scope workspace` there to \
                 fire it again, or delete '{}'",
                path.display()
            ),
            (InactivePluginScope::Host, _) => format!(
                "seeded by plugin:{namespace}@{version}, which is not enabled on this host; run \
                 `orbit plugin enable {namespace}` to fire it again, or delete '{}'",
                path.display()
            ),
        }
    }
}
