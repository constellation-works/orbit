//! `orbit plugin list` and `show`: one projected summary per plugin.

use std::collections::BTreeMap;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_tools::plugin::{
    LoadedPlugin, PluginProgramStatus, mark_resolution_drift, program_statuses,
};
use orbit_types::plugin::{
    InstalledPlugin, PluginBuildRecord, PluginDisabledLayer, PluginExecutionKind, PluginGrant,
    PluginSandbox, PluginStatus, plugin_tool_name,
};
use serde_json::{Value, json};

use super::super::panels::{PluginLinkSummary, PluginPanelSummary, web_summaries};
use super::profile::permission_rows;

use crate::OrbitRuntime;
use crate::runtime::plugin::cache::load_installed_plugin;
use crate::runtime::plugin::grants::recorded_program_paths;
use crate::runtime::plugin::paths::{plugin_state_dir, read_pin_file};

/// One plugin tool as the CLI reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginToolSummary {
    /// Canonical registry name (`<ns>.<verb>` or `orbit.<ns>.<verb>`).
    pub name: String,
    /// MCP-advertised name, absent when `mcp_scope: none`.
    pub advertised_name: Option<String>,
    pub execution_kind: PluginExecutionKind,
    /// Whether the manifest requests consented agent-session mutation.
    pub agent_callable: bool,
    pub mcp_scope: String,
    pub active: bool,
}

/// One grant as `orbit plugin show` reports it: what the manifest asks for
/// beside whether the operator granted it (design §4.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginPermissionSummary {
    pub grant: PluginGrant,
    /// The manifest's request, `None` when it does not ask for this grant.
    pub requested: Option<String>,
    pub granted: bool,
    /// The roots the operator scoped this grant to, `None` when the grant is
    /// unscoped — either not granted at all, or granted as the whole request
    /// the manifest makes. Reading it beside `requested` is how a surface
    /// shows the delta: the manifest asks for these paths, the operator
    /// allowed those, and the sandbox opens the intersection.
    pub granted_roots: Option<Vec<String>>,
}

/// One plugin as `orbit plugin list` / `show` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginSummary {
    pub name: String,
    pub version: String,
    pub status: PluginStatus,
    pub source: String,
    pub install_path: String,
    pub manifest_digest: String,
    pub publisher: Option<String>,
    pub description: String,
    pub first_party: bool,
    /// Every grant, with the manifest's request and the operator's answer
    /// side by side (§4.1). Only the answer is authority.
    pub permissions: Vec<PluginPermissionSummary>,
    /// Grants recorded at `orbit plugin enable --grant …`.
    pub granted: Vec<String>,
    /// `backend.sandbox: none` with the `unsandboxed` grant: the backend runs
    /// unconfined, which `doctor` reports as a finding (§4.3).
    pub unsandboxed: bool,
    /// Every `requires.programs` entry beside the path the last enabling
    /// command resolved it to, and why the sandbox will not grant it when it
    /// will not (§4.3).
    pub programs: Vec<PluginProgramStatus>,
    pub tools: Vec<PluginToolSummary>,
    /// `spec.web.panels[]` of an active plugin (§4.7). Empty for a plugin
    /// that is not serving its tools: a panel reads one of them.
    pub panels: Vec<PluginPanelSummary>,
    /// `spec.web.links[]`, with `{{config.<key>}}` resolved.
    pub links: Vec<PluginLinkSummary>,
    /// The Orbit version this plugin's conformance goldens last passed on
    /// (§5), when `orbit plugin test` has recorded one.
    pub certified_orbit_version: Option<String>,
    /// What this host recorded about the install-time build that produced
    /// the plugin's outputs (design `docs/design/plugins/3_install_time_build.md`
    /// §3.6), when it was built here.
    pub build: Option<PluginBuildRecord>,
    /// Why the plugin is not active, when it is not.
    pub diagnostic: Option<String>,
    /// Whether `.orbit/plugins.yaml` pins this plugin.
    pub pinned: bool,
    /// The host row's enable state (`orbit plugin enable|disable`). False
    /// for a plugin this host has not installed.
    pub host_enabled: bool,
    /// This workspace's `[plugin_enablement]` toggle, when it sets one. An
    /// absent toggle inherits the host state.
    pub workspace_toggle: Option<bool>,
    /// For a [`PluginStatus::Disabled`] plugin, the layer that switched it
    /// off — the reason the effective state is what it is.
    pub disabled_by: Option<PluginDisabledLayer>,
}

/// Surface-specific presentation of a [`PluginSummary`] JSON record.
#[derive(Debug, Clone, Copy)]
pub enum PluginSummaryJsonProfile<'a> {
    /// Filesystem paths remain usable by CLI callers.
    Cli,
    /// Display paths abbreviate the home directory; host metadata comes from
    /// the build serving the HTTP response.
    Http {
        /// Home directory to abbreviate, when known.
        home: Option<&'a Path>,
        /// Version of the HTTP server build.
        host_orbit_version: &'a str,
    },
}

impl PluginSummaryJsonProfile<'_> {
    fn display_path(self, value: &str) -> String {
        if let Self::Http {
            home: Some(home), ..
        } = self
        {
            let path = Path::new(value);
            if path == home {
                return "~".to_string();
            }
            if let Ok(rest) = path.strip_prefix(home) {
                return format!("~/{}", rest.display());
            }
        }
        value.to_string()
    }
}

/// Project a plugin record for CLI output or the HTTP API.
///
/// Both profiles share plugin data; HTTP additionally carries display paths,
/// the effective enable flag and the serving build's version. Session
/// authorization capabilities belong to the HTTP handler.
pub fn plugin_summary_json(
    summary: &PluginSummary,
    profile: PluginSummaryJsonProfile<'_>,
) -> Value {
    let mut value = json!({
        "name": summary.name,
        "version": summary.version,
        "status": summary.status.as_str(),
        "source": profile.display_path(&summary.source),
        "install_path": profile.display_path(&summary.install_path),
        "manifest_digest": summary.manifest_digest,
        "publisher": summary.publisher,
        "description": summary.description,
        "first_party": summary.first_party,
        "pinned": summary.pinned,
        "host_enabled": summary.host_enabled,
        "workspace_toggle": summary.workspace_toggle,
        "disabled_by": summary.disabled_by.map(PluginDisabledLayer::as_str),
        "permissions": summary
            .permissions
            .iter()
            .map(|permission| json!({
                "grant": permission.grant.as_str(),
                "requested": permission.requested,
                "granted": permission.granted,
                "granted_roots": permission.granted_roots,
            }))
            .collect::<Vec<_>>(),
        "granted": summary.granted,
        "unsandboxed": summary.unsandboxed,
        "programs": summary
            .programs
            .iter()
            .map(|program| json!({
                "name": program.name,
                "path": program.path.as_ref().map(|path| profile.display_path(&path.to_string_lossy())),
                "granted": program.granted(),
                "problem": program.problem,
                "resolves_to": program.resolves_to.as_ref().map(|path| profile.display_path(&path.to_string_lossy())),
            }))
            .collect::<Vec<_>>(),
        "certified_orbit_version": summary.certified_orbit_version,
        "build": summary.build,
        "diagnostic": summary.diagnostic,
        "panels": summary
            .panels
            .iter()
            .map(|panel| json!({
                "id": panel.id,
                "title": panel.title,
                "tool": panel.tool,
                "render": panel.render.as_str(),
                "group": panel.group.as_str(),
                "refresh_ms": panel.refresh_ms,
            }))
            .collect::<Vec<_>>(),
        "links": summary
            .links
            .iter()
            .map(|link| json!({ "title": link.title, "url": link.url }))
            .collect::<Vec<_>>(),
        "tools": summary
            .tools
            .iter()
            .map(|tool| json!({
                "name": tool.name,
                "advertised_name": tool.advertised_name,
                "execution_kind": tool.execution_kind.as_str(),
                "agent_callable": tool.agent_callable,
                "mcp_scope": tool.mcp_scope,
                "active": tool.active,
            }))
            .collect::<Vec<_>>(),
    });
    if let PluginSummaryJsonProfile::Http {
        host_orbit_version, ..
    } = profile
    {
        value["enabled"] = json!(summary.status == PluginStatus::Active);
        value["host_orbit_version"] = json!(host_orbit_version);
    }
    value
}

pub fn list_plugins(runtime: &OrbitRuntime) -> Result<Vec<PluginSummary>, OrbitError> {
    let pinned = pinned_names(runtime);
    let mut summaries: Vec<PluginSummary> = runtime
        .stores()
        .plugins()
        .list_plugins()?
        .iter()
        .map(|installed| {
            let mut summary = summary_from_runtime(runtime, installed);
            summary.pinned = pinned.contains(&summary.name);
            summary
        })
        .collect();
    // A pin this host never installed has no record, and is exactly what an
    // operator needs to see here.
    for name in pinned {
        if summaries.iter().any(|summary| summary.name == name) {
            continue;
        }
        summaries.push(PluginSummary {
            name: name.clone(),
            version: String::new(),
            status: PluginStatus::Missing,
            source: String::new(),
            install_path: String::new(),
            manifest_digest: String::new(),
            publisher: None,
            description: String::new(),
            first_party: false,
            permissions: Vec::new(),
            granted: Vec::new(),
            unsandboxed: false,
            programs: Vec::new(),
            tools: Vec::new(),
            panels: Vec::new(),
            links: Vec::new(),
            certified_orbit_version: None,
            build: None,
            diagnostic: runtime_diagnostic(runtime, &name),
            pinned: true,
            host_enabled: false,
            workspace_toggle: runtime.plugin_load().workspace_toggles.get(&name).copied(),
            disabled_by: None,
        });
    }
    summaries.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(summaries)
}

pub fn show_plugin(runtime: &OrbitRuntime, name: &str) -> Result<PluginSummary, OrbitError> {
    list_plugins(runtime)?
        .into_iter()
        .find(|summary| summary.name == name)
        .ok_or_else(|| {
            OrbitError::InvalidInput(format!(
                "plugin '{name}' is neither installed on this host nor pinned by this workspace"
            ))
        })
}

/// Summary of an installed plugin using this runtime's own load outcome, so
/// the report and the live tool surface cannot disagree.
fn summary_from_runtime(runtime: &OrbitRuntime, installed: &InstalledPlugin) -> PluginSummary {
    let registered = runtime
        .plugin_load()
        .registered
        .iter()
        .find(|entry| entry.name == installed.name);
    let status = registered.map_or_else(
        || {
            if installed.enabled {
                PluginStatus::Inactive
            } else {
                PluginStatus::Disabled
            }
        },
        |entry| entry.status,
    );
    // Disabled rows are deliberately not loaded during ordinary runtime
    // construction. `plugin list` still reports their manifest details, but
    // only this command pays that one cached load.
    let disabled = if installed.enabled {
        None
    } else {
        load_installed_plugin(installed).ok()
    };
    let loaded = registered
        .and_then(|entry| entry.loaded.as_deref())
        .or(disabled.as_deref());
    let mut summary = summary_for_installed(installed, loaded, status, &runtime.global_root());
    summary.workspace_toggle = runtime
        .plugin_load()
        .workspace_toggles
        .get(&installed.name)
        .copied();
    if let Some(entry) = registered.filter(|entry| entry.status == PluginStatus::Disabled) {
        summary.disabled_by = entry.disabled_by;
    }
    // Panels and links are the *active* surface, so they are projected from
    // the load pass that built it — including the effective `[plugins.<ns>]`
    // values a link template reads — rather than from the manifest alone.
    if let Some(entry) = registered.filter(|entry| entry.status == PluginStatus::Active)
        && let Some(plugin) = entry.loaded.as_ref()
    {
        let (panels, links) = web_summaries(plugin, installed.first_party, &entry.config_values);
        summary.panels = panels;
        summary.links = links;
    }
    // A row whose grants this host could not verify granted nothing, so the
    // report says so rather than repeating the row's claim back as authority —
    // otherwise `orbit plugin show` would print `unsandboxed` for a plugin the
    // loader refused to run at all [ORB-12778].
    if registered.is_some_and(|entry| !entry.grants_authorized) {
        summary.granted.clear();
        summary.unsandboxed = false;
        for permission in &mut summary.permissions {
            permission.granted = false;
            permission.granted_roots = None;
        }
    }
    summary.diagnostic = registered.and_then(|entry| entry.diagnostic.clone());
    if summary.diagnostic.is_none() && status == PluginStatus::Inactive && loaded.is_none() {
        summary.diagnostic = Some(format!(
            "plugin '{}' no longer loads from {}; reinstall it with `orbit plugin add`",
            installed.name, installed.install_path
        ));
    }
    summary
}

fn runtime_diagnostic(runtime: &OrbitRuntime, name: &str) -> Option<String> {
    runtime
        .plugin_load()
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.plugin == name)
        .map(|diagnostic| diagnostic.message.clone())
}

/// Shared projection of one installed plugin plus, when it loads, its manifest.
pub(in crate::application::plugin) fn summary_for_installed(
    installed: &InstalledPlugin,
    plugin: Option<&LoadedPlugin>,
    status: PluginStatus,
    global_root: &Path,
) -> PluginSummary {
    let (panels, links) = plugin
        .filter(|_| status == PluginStatus::Active)
        .map(|plugin| web_summaries(plugin, installed.first_party, &BTreeMap::new()))
        .unwrap_or_default();
    let tools = plugin
        .map(|plugin| {
            plugin
                .tools
                .iter()
                .map(|tool| {
                    let name =
                        plugin_tool_name(plugin.namespace(), &tool.verb, installed.first_party);
                    let advertised = match tool.mcp_scope {
                        orbit_types::plugin::PluginMcpScope::None => None,
                        _ => Some(orbit_types::tool::mcp_advertised_tool_name(&name)),
                    };
                    PluginToolSummary {
                        name,
                        advertised_name: advertised,
                        execution_kind: tool.execution_kind,
                        agent_callable: tool.agent_callable,
                        mcp_scope: mcp_scope_label(tool.mcp_scope).to_string(),
                        active: status == PluginStatus::Active,
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    PluginSummary {
        name: installed.name.clone(),
        version: installed.version.clone(),
        status,
        source: installed.source.clone(),
        install_path: installed.install_path.clone(),
        manifest_digest: installed.manifest_digest.clone(),
        publisher: plugin.and_then(|plugin| plugin.manifest.metadata.publisher.clone()),
        description: plugin
            .map(|plugin| plugin.manifest.metadata.description.clone())
            .unwrap_or_default(),
        first_party: installed.first_party,
        permissions: plugin
            .map(|plugin| permission_rows(plugin, &installed.grants))
            .unwrap_or_default(),
        granted: installed.grants.clone(),
        unsandboxed: plugin.is_some_and(|plugin| {
            plugin.manifest.spec.backend.sandbox == PluginSandbox::None
                && installed
                    .grants
                    .iter()
                    .any(|grant| grant == PluginGrant::Unsandboxed.as_str())
        }),
        programs: plugin
            .map(|plugin| {
                let mut programs = program_statuses(
                    &plugin.manifest.spec.requires.programs,
                    &recorded_program_paths(global_root, &installed.name),
                    global_root,
                    &plugin_state_dir(global_root, &installed.name),
                );
                // Inspection reads the operator's PATH; the call path never does.
                mark_resolution_drift(&mut programs, std::env::var_os("PATH").as_deref());
                programs
            })
            .unwrap_or_default(),
        tools,
        panels,
        links,
        certified_orbit_version: installed.certified_orbit_version.clone(),
        build: installed.build.clone(),
        diagnostic: None,
        pinned: false,
        host_enabled: installed.enabled,
        workspace_toggle: None,
        disabled_by: (status == PluginStatus::Disabled && !installed.enabled)
            .then_some(PluginDisabledLayer::Host),
    }
}

fn mcp_scope_label(scope: orbit_types::plugin::PluginMcpScope) -> &'static str {
    match scope {
        orbit_types::plugin::PluginMcpScope::Workspace => "workspace",
        orbit_types::plugin::PluginMcpScope::Global => "global",
        orbit_types::plugin::PluginMcpScope::None => "none",
    }
}

fn pinned_names(runtime: &OrbitRuntime) -> Vec<String> {
    read_pin_file(&runtime.shared_root())
        .ok()
        .flatten()
        .map(|pins| {
            pins.plugins
                .into_iter()
                .map(|pin| pin.name)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}
