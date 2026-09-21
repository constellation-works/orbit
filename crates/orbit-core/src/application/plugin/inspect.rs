//! Read-only plugin surfaces: `list`, `show`, `doctor` and `validate`.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_tools::plugin::{
    LoadedPlugin, PluginValidationPolicy, load_plugin_dir, manifest_refusal, validate_loaded_plugin,
};
use orbit_types::plugin::{InstalledPlugin, PluginExecutionKind, PluginStatus, plugin_tool_name};

use crate::OrbitRuntime;
use crate::runtime::plugin_host::{read_pin_file, unmet_requirement};

/// One plugin tool as the CLI reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginToolSummary {
    /// Canonical registry name (`<ns>.<verb>` or `orbit.<ns>.<verb>`).
    pub name: String,
    /// MCP-advertised name, absent when `mcp_scope: none`.
    pub advertised_name: Option<String>,
    pub execution_kind: PluginExecutionKind,
    pub mcp_scope: String,
    pub active: bool,
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
    /// Permissions the manifest requests (§4.1). Never a grant.
    pub requested_permissions: Vec<String>,
    /// Grants recorded at `orbit plugin enable --grant …`.
    pub granted: Vec<String>,
    pub tools: Vec<PluginToolSummary>,
    /// Why the plugin is not active, when it is not.
    pub diagnostic: Option<String>,
    /// Whether `.orbit/plugins.yaml` pins this plugin.
    pub pinned: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginDoctorResult {
    pub plugin: String,
    pub status: PluginStatus,
    /// The step an operator has to take, or empty when there is none.
    pub message: String,
}

/// What `orbit plugin validate <dir>` found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginValidationReport {
    pub name: String,
    pub version: String,
    pub root: String,
    pub manifest_digest: String,
    pub tools: Vec<String>,
    /// Non-fatal observations: a `requires` this host does not satisfy, a
    /// first-party claim this source cannot support, sections parsed but not
    /// yet consumed.
    pub warnings: Vec<String>,
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
            requested_permissions: Vec::new(),
            granted: Vec::new(),
            tools: Vec::new(),
            diagnostic: runtime_diagnostic(runtime, &name),
            pinned: true,
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

/// One row per plugin, naming the step that would make it active.
pub fn plugin_doctor(runtime: &OrbitRuntime) -> Result<Vec<PluginDoctorResult>, OrbitError> {
    Ok(list_plugins(runtime)?
        .into_iter()
        .map(|summary| {
            let message = summary.diagnostic.clone().unwrap_or_else(|| match summary.status {
                PluginStatus::Active => String::new(),
                PluginStatus::Disabled => format!(
                    "plugin '{}' is installed but disabled; run `orbit plugin enable {}`",
                    summary.name, summary.name
                ),
                PluginStatus::Missing => format!(
                    "plugin '{}' is pinned by this workspace but not installed on this host; run \
                     `orbit plugin sync`",
                    summary.name
                ),
                PluginStatus::Inactive => format!(
                    "plugin '{}' is enabled but was refused at load; run `orbit plugin show {}`",
                    summary.name, summary.name
                ),
            });
            PluginDoctorResult {
                plugin: summary.name,
                status: summary.status,
                message,
            }
        })
        .collect())
}

/// Validate a plugin directory without installing it.
pub fn validate_plugin_dir(
    runtime: &OrbitRuntime,
    dir: &Path,
    first_party_verified: bool,
) -> Result<PluginValidationReport, OrbitError> {
    let plugin = load_plugin_dir(dir)?;
    let policy =
        PluginValidationPolicy::host_default().with_first_party_verified(first_party_verified);
    validate_loaded_plugin(&plugin, &policy).map_err(manifest_refusal)?;

    let mut warnings = Vec::new();
    if let Some(message) = unmet_requirement(&plugin) {
        warnings.push(message);
    }
    if plugin.manifest.spec.definitions.is_some()
        || plugin.manifest.spec.web.is_some()
        || plugin.manifest.spec.config.is_some()
        || !plugin.manifest.spec.skills.is_empty()
        || !plugin.manifest.spec.tests.is_empty()
    {
        warnings.push(
            "`definitions`, `skills`, `config`, `web` and `tests` are accepted by this Orbit but \
             not yet installed; only `spec.tools` reaches the tool surface today"
                .to_string(),
        );
    }
    if !plugin.manifest.spec.permissions.orbit_tools.is_empty()
        || plugin.manifest.spec.permissions.network
            != orbit_types::plugin::PluginNetworkPermission::None
        || !plugin.manifest.spec.permissions.fs.read.is_empty()
        || !plugin.manifest.spec.permissions.fs.write.is_empty()
    {
        warnings.push(
            "`spec.permissions` is recorded as requested; grants are not enforced by this Orbit \
             release"
                .to_string(),
        );
    }
    let _ = runtime;
    Ok(PluginValidationReport {
        name: plugin.namespace().to_string(),
        version: plugin.manifest.metadata.version.clone(),
        root: plugin.root.to_string_lossy().into_owned(),
        manifest_digest: plugin.manifest_digest.clone(),
        tools: plugin
            .tools
            .iter()
            .map(|tool| {
                plugin_tool_name(
                    plugin.namespace(),
                    &tool.verb,
                    plugin.manifest.claims_first_party_namespace(),
                )
            })
            .collect(),
        warnings,
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
    let loaded = load_plugin_dir(Path::new(&installed.install_path)).ok();
    let mut summary = summary_for_installed(installed, loaded.as_ref(), status);
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
pub(super) fn summary_for_installed(
    installed: &InstalledPlugin,
    plugin: Option<&LoadedPlugin>,
    status: PluginStatus,
) -> PluginSummary {
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
        requested_permissions: plugin.map(requested_permissions).unwrap_or_default(),
        granted: installed.grants.clone(),
        tools,
        diagnostic: None,
        pinned: false,
    }
}

fn mcp_scope_label(scope: orbit_types::plugin::PluginMcpScope) -> &'static str {
    match scope {
        orbit_types::plugin::PluginMcpScope::Workspace => "workspace",
        orbit_types::plugin::PluginMcpScope::Global => "global",
        orbit_types::plugin::PluginMcpScope::None => "none",
    }
}

/// The manifest's requests, rendered for `orbit plugin show`'s
/// requested-vs-granted columns.
fn requested_permissions(plugin: &LoadedPlugin) -> Vec<String> {
    let permissions = &plugin.manifest.spec.permissions;
    let mut requested = Vec::new();
    if !permissions.fs.read.is_empty() {
        requested.push(format!("fs.read={}", permissions.fs.read.join(",")));
    }
    if !permissions.fs.write.is_empty() {
        requested.push(format!("fs.write={}", permissions.fs.write.join(",")));
    }
    if permissions.network != orbit_types::plugin::PluginNetworkPermission::None {
        requested.push(format!("network={:?}", permissions.network).to_lowercase());
    }
    if !permissions.env_pass.is_empty() {
        requested.push(format!("env_pass={}", permissions.env_pass.join(",")));
    }
    if !permissions.orbit_tools.is_empty() {
        requested.push(format!("orbit_tools={}", permissions.orbit_tools.join(",")));
    }
    if plugin.manifest.spec.backend.sandbox == orbit_types::plugin::PluginSandbox::None {
        requested.push("unsandboxed".to_string());
    }
    requested
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
