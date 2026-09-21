//! Register installed plugins into the runtime tool registry.
//!
//! Fail closed per plugin (design `docs/design/plugins/1_scope.md` §4.9): a
//! plugin whose manifest no longer loads, whose `requires` no longer hold,
//! whose namespace collides, or whose required grants the operator has not
//! recorded is reported as one diagnostic and registered inactive; every
//! built-in and every other plugin is untouched.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;

use orbit_common::OrbitError;
use orbit_store::Store;
use orbit_tools::ToolRegistry;
use orbit_tools::plugin::{
    LoadedPlugin, McpBackend, McpExpectedTool, PluginBackend, PluginBackendSpec, PluginTool,
    PluginToolBinding, PluginValidationPolicy, load_plugin_dir, validate_loaded_plugin,
};
use orbit_types::plugin::{
    InstalledPlugin, PLUGIN_HOST_API, PluginBackendType, PluginGrant, PluginMcpScope,
    PluginPinFile, PluginProvenance, PluginStatus, SemverRange, Version, plugin_tool_name,
};
use orbit_types::tool::{McpToolDefinition, McpToolScope};

/// Why a plugin is not on the active tool surface, and what would fix it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginDiagnostic {
    pub plugin: String,
    pub status: PluginStatus,
    pub message: String,
}

/// What the runtime registered for one host plugin.
#[derive(Debug, Clone, PartialEq)]
pub struct RegisteredPlugin {
    pub name: String,
    pub version: String,
    pub status: PluginStatus,
    /// Canonical tool names, active or inactive.
    pub tools: Vec<String>,
    pub diagnostic: Option<String>,
    /// The manifest this pass loaded, when it loaded at all. Retained so the
    /// catalog layer, the seeded definitions and the config contract all read
    /// the same document the tool surface was built from.
    pub loaded: Option<Arc<LoadedPlugin>>,
}

/// Outcome of a host plugin load pass.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PluginHostLoad {
    pub registered: Vec<RegisteredPlugin>,
    pub diagnostics: Vec<PluginDiagnostic>,
}

impl PluginHostLoad {
    /// Every plugin whose tools and definitions are on the active surface.
    pub fn active(&self) -> impl Iterator<Item = &Arc<LoadedPlugin>> {
        self.registered
            .iter()
            .filter(|entry| entry.status == PluginStatus::Active)
            .filter_map(|entry| entry.loaded.as_ref())
    }

    /// Whether a plugin of this namespace is active on the host.
    pub fn is_active(&self, namespace: &str) -> bool {
        self.active().any(|plugin| plugin.namespace() == namespace)
    }
}

/// Check everything a plugin contributes beyond its tools: the definition
/// rules of §4.5 and its own `[plugins.<ns>]` schema.
///
/// One message, naming the file or key at fault, because the caller reports it
/// as this plugin's single diagnostic.
pub(crate) fn validate_plugin_contributions(
    plugin: &LoadedPlugin,
    plugin_config: &BTreeMap<String, Value>,
) -> Result<(), String> {
    super::plugin_definitions::load_plugin_definitions(
        plugin,
        &super::plugin_definitions::shipped_job_names(),
    )?;
    super::plugin_config::validate_plugin_config(plugin, plugin_config)
}

/// Where a plugin lives on this host: `<global>/plugins/<ns>/<version>`.
pub fn plugin_install_root(global_root: &Path) -> PathBuf {
    global_root.join("plugins")
}

pub fn plugin_install_path(global_root: &Path, name: &str, version: &str) -> PathBuf {
    plugin_install_root(global_root).join(name).join(version)
}

/// The `current` link a host keeps beside the versioned install directories.
pub fn plugin_current_link(global_root: &Path, name: &str) -> PathBuf {
    plugin_install_root(global_root).join(name).join("current")
}

/// Per-plugin state directory handed to the backend as `ORBIT_PLUGIN_STATE`.
pub fn plugin_state_dir(global_root: &Path, name: &str) -> PathBuf {
    global_root.join("state").join("plugins").join(name)
}

/// The workspace's committed pin file, when it has one.
pub fn read_pin_file(orbit_dir: &Path) -> Result<Option<PluginPinFile>, OrbitError> {
    let path = orbit_dir.join(orbit_types::plugin::PIN_FILE_NAME);
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Ok(None);
    };
    let pins: PluginPinFile = serde_yaml::from_str(&raw).map_err(|error| {
        OrbitError::InvalidInput(format!("invalid {}: {error}", path.display()))
    })?;
    pins.validate()
        .map_err(|error| OrbitError::InvalidInput(format!("{}: {error}", path.display())))?;
    Ok(Some(pins))
}

/// Every active plugin tool this host serves, with the scope its manifest
/// declares.
///
/// Host-global by construction: plugin installs are per host, so this answers
/// without a workspace runtime and is what lets the MCP surface advertise
/// plugin tools to a session that has not named a workspace yet.
pub fn host_plugin_mcp_definitions(
    global_root: &Path,
    audit_db: &Path,
) -> Result<Vec<McpToolDefinition>, OrbitError> {
    let registry = host_plugin_registry(global_root, audit_db)?.0;
    registry
        .mcp_tool_definitions()
        .map_err(|error| OrbitError::InvalidInput(error.to_string()))
}

/// A registry holding this host's plugin tools and nothing else.
///
/// Namespace validation still runs against the real built-in names, so a
/// colliding plugin is refused here exactly as it is in a workspace runtime.
pub fn host_plugin_registry(
    global_root: &Path,
    audit_db: &Path,
) -> Result<(ToolRegistry, PluginHostLoad), OrbitError> {
    let store = Store::open_read_only(audit_db)?;
    let mut registry = ToolRegistry::new();
    // No workspace here, so no pin file: the global root holds none, and an
    // absent pin file is a valid configuration.
    let load = load_host_plugins(
        global_root,
        global_root,
        &store,
        &mut registry,
        &BTreeMap::new(),
    );
    Ok((registry, load))
}

/// Register every enabled installed plugin, plus one diagnostic per plugin
/// the workspace pins that this host cannot serve.
pub fn load_host_plugins(
    global_root: &Path,
    orbit_dir: &Path,
    store: &Store,
    registry: &mut ToolRegistry,
    plugin_config: &BTreeMap<String, Value>,
) -> PluginHostLoad {
    let installed = match store.list_plugins() {
        Ok(installed) => installed,
        Err(error) => {
            return PluginHostLoad {
                registered: Vec::new(),
                diagnostics: vec![PluginDiagnostic {
                    plugin: String::new(),
                    status: PluginStatus::Inactive,
                    message: format!("cannot read the host plugin records: {error}"),
                }],
            };
        }
    };

    let mut load = PluginHostLoad::default();
    let policy = PluginValidationPolicy::host_default();
    for plugin in &installed {
        let registered =
            register_installed_plugin(global_root, plugin, &policy, registry, plugin_config);
        if let Some(message) = &registered.diagnostic {
            load.diagnostics.push(PluginDiagnostic {
                plugin: plugin.name.clone(),
                status: registered.status,
                message: message.clone(),
            });
        }
        load.registered.push(registered);
    }

    // A pin this host has not installed cannot contribute tools — their names
    // live in a manifest that is not here. Report it once so `plugin list`,
    // `show` and `doctor` all name the missing step.
    match read_pin_file(orbit_dir) {
        Ok(Some(pins)) => {
            for pin in pins.plugins.iter().filter(|pin| pin.enabled) {
                if installed.iter().any(|plugin| plugin.name == pin.name) {
                    continue;
                }
                let source = pin
                    .source
                    .clone()
                    .unwrap_or_else(|| "<no source pinned>".to_string());
                load.diagnostics.push(PluginDiagnostic {
                    plugin: pin.name.clone(),
                    status: PluginStatus::Missing,
                    message: format!(
                        "plugin '{}' is pinned by this workspace but is not installed on this \
                         host; run `orbit plugin sync` or `orbit plugin add {source}`",
                        pin.name
                    ),
                });
            }
        }
        Ok(None) => {}
        Err(error) => load.diagnostics.push(PluginDiagnostic {
            plugin: String::new(),
            status: PluginStatus::Inactive,
            message: error.to_string(),
        }),
    }

    load
}

fn register_installed_plugin(
    global_root: &Path,
    installed: &InstalledPlugin,
    policy: &PluginValidationPolicy,
    registry: &mut ToolRegistry,
    plugin_config: &BTreeMap<String, Value>,
) -> RegisteredPlugin {
    let refused = |status: PluginStatus, message: String| RegisteredPlugin {
        name: installed.name.clone(),
        version: installed.version.clone(),
        status,
        tools: Vec::new(),
        diagnostic: Some(message),
        loaded: None,
    };

    if !installed.enabled {
        return RegisteredPlugin {
            name: installed.name.clone(),
            version: installed.version.clone(),
            status: PluginStatus::Disabled,
            tools: Vec::new(),
            diagnostic: None,
            loaded: None,
        };
    }

    let plugin = match load_plugin_dir(Path::new(&installed.install_path)) {
        Ok(plugin) => plugin,
        Err(error) => {
            return refused(
                PluginStatus::Inactive,
                format!(
                    "plugin '{}' no longer loads from {}: {error}; reinstall it with `orbit \
                     plugin add`",
                    installed.name, installed.install_path
                ),
            );
        }
    };
    let policy = policy
        .clone()
        .with_first_party_verified(installed.first_party);
    if let Err(error) = validate_loaded_plugin(&plugin, &policy) {
        return refused(
            PluginStatus::Inactive,
            format!("plugin '{}' is refused: {error}", installed.name),
        );
    }
    if let Some(message) = unmet_requirement(&plugin) {
        return register_inactive_tools(global_root, installed, &plugin, registry, message);
    }
    if let Some(message) = missing_grant_diagnostic(installed, &plugin) {
        return register_inactive_tools(global_root, installed, &plugin, registry, message);
    }
    // A plugin whose shipped definitions break the §4.5 rules, or whose
    // `[plugins.<ns>]` section its own schema rejects, contributes nothing:
    // registering its tools while its catalog layer is unusable would leave
    // half a plugin on the surface.
    if let Err(message) = validate_plugin_contributions(&plugin, plugin_config) {
        return refused(
            PluginStatus::Inactive,
            format!("plugin '{}' is refused: {message}", installed.name),
        );
    }

    let backend = plugin_backend(global_root, installed, &plugin, plugin_config);
    let provenance = backend.spec().provenance.clone();
    let mut tools = Vec::with_capacity(plugin.tools.len());
    for tool in &plugin.tools {
        let name = plugin_tool_name(plugin.namespace(), &tool.verb, installed.first_party);
        let binding = Arc::new(PluginToolBinding {
            provenance: provenance.clone(),
            execution_kind: tool.execution_kind,
            diagnostic: None,
        });
        let scope = match tool.mcp_scope {
            PluginMcpScope::Workspace => Some(McpToolScope::WorkspaceRequired),
            PluginMcpScope::Global => Some(McpToolScope::Global),
            PluginMcpScope::None => None,
        };
        registry.register_plugin_tool(
            plugin_tool(&plugin, tool, &name, binding.clone(), backend.clone()),
            scope,
            binding,
        );
        tools.push(name);
    }
    RegisteredPlugin {
        name: installed.name.clone(),
        version: installed.version.clone(),
        status: PluginStatus::Active,
        tools,
        diagnostic: None,
        loaded: Some(Arc::new(plugin)),
    }
}

/// An enabled plugin whose `requires` no longer hold keeps its tool names on
/// the registry as inactive entries, so a caller that names one is told why
/// rather than told the tool does not exist (§4.8).
fn register_inactive_tools(
    global_root: &Path,
    installed: &InstalledPlugin,
    plugin: &LoadedPlugin,
    registry: &mut ToolRegistry,
    message: String,
) -> RegisteredPlugin {
    let backend = plugin_backend(global_root, installed, plugin, &BTreeMap::new());
    let provenance = backend.spec().provenance.clone();
    let mut tools = Vec::with_capacity(plugin.tools.len());
    for tool in &plugin.tools {
        let name = plugin_tool_name(plugin.namespace(), &tool.verb, installed.first_party);
        let binding = Arc::new(PluginToolBinding {
            provenance: provenance.clone(),
            execution_kind: tool.execution_kind,
            diagnostic: Some(message.clone()),
        });
        registry.register_inactive_plugin_tool(
            plugin_tool(plugin, tool, &name, binding.clone(), backend.clone()),
            binding,
        );
        tools.push(name);
    }
    RegisteredPlugin {
        name: installed.name.clone(),
        version: installed.version.clone(),
        status: PluginStatus::Inactive,
        tools,
        diagnostic: Some(message),
        loaded: None,
    }
}

/// A required grant the operator has not recorded refuses the whole plugin,
/// naming the grant, the manifest key that asks for it, and the command that
/// records it (design §4.1). `backend.sandbox: none` is the `unsandboxed`
/// grant, so it is refused here too.
pub fn missing_grant_diagnostic(
    installed: &InstalledPlugin,
    plugin: &LoadedPlugin,
) -> Option<String> {
    let missing = plugin.manifest.missing_grants(&installed.grants);
    if missing.is_empty() {
        return None;
    }
    let asks = missing
        .iter()
        .map(|grant| format!("`{grant}` ({})", grant.requested_by()))
        .collect::<Vec<_>>()
        .join(", ");
    let flags = missing
        .iter()
        .map(|grant| grant.as_str())
        .collect::<Vec<_>>()
        .join(",");
    Some(format!(
        "plugin '{}' requests {asks} but this host has not granted {}; run `orbit plugin enable {} \
         --grant {flags}` to grant {}",
        installed.name,
        if missing.len() == 1 { "it" } else { "them" },
        installed.name,
        if missing.len() == 1 { "it" } else { "them" },
    ))
}

/// The backend every tool of this plugin shares: the spec for `exec`, or one
/// long-lived server proxy for `mcp` (design §4.2).
fn plugin_backend(
    global_root: &Path,
    installed: &InstalledPlugin,
    plugin: &LoadedPlugin,
    plugin_config: &BTreeMap<String, Value>,
) -> PluginBackend {
    let grants: Vec<PluginGrant> = installed
        .grants
        .iter()
        .filter_map(|name| PluginGrant::parse(name))
        .collect();
    // `{{config.<key>}}` resolves against the effective section: what the
    // operator configured in `[plugins.<ns>]`, over what the manifest
    // defaults (§1).
    let config_defaults = super::plugin_config::plugin_config_values(plugin, plugin_config);
    let spec = Arc::new(PluginBackendSpec {
        provenance: PluginProvenance {
            name: installed.name.clone(),
            version: installed.version.clone(),
            manifest_digest: installed.manifest_digest.clone(),
            grants: grants
                .iter()
                .map(|grant| grant.as_str().to_string())
                .collect(),
        },
        plugin_root: plugin.root.clone(),
        state_dir: plugin_state_dir(global_root, &installed.name),
        global_root: global_root.to_path_buf(),
        command: plugin.backend_command.clone(),
        args: plugin.manifest.spec.backend.args.clone(),
        timeout_ms: plugin.manifest.spec.backend.timeout_ms,
        sandbox: plugin.manifest.spec.backend.sandbox,
        permissions: plugin.manifest.spec.permissions.clone(),
        programs: plugin.manifest.spec.requires.programs.clone(),
        config_defaults,
        grants,
    });
    match plugin.manifest.spec.backend.backend_type {
        PluginBackendType::Exec => PluginBackend::Exec(spec),
        PluginBackendType::Mcp => {
            let expected = plugin
                .tools
                .iter()
                .map(|tool| McpExpectedTool {
                    verb: tool.verb.clone(),
                    input_schema: tool
                        .input_schema_declared
                        .then(|| tool.input_schema.clone()),
                })
                .collect();
            PluginBackend::Mcp(Arc::new(McpBackend::new(spec, expected)))
        }
    }
}

fn plugin_tool(
    plugin: &LoadedPlugin,
    tool: &orbit_tools::plugin::ResolvedPluginTool,
    name: &str,
    binding: Arc<PluginToolBinding>,
    backend: PluginBackend,
) -> PluginTool {
    PluginTool {
        name: name.to_string(),
        verb: tool.verb.clone(),
        description: plugin_tool_description(plugin, tool),
        parameters: tool.parameters.clone(),
        execution_kind: tool.execution_kind,
        output_schema: tool.output_schema.clone(),
        binding,
        backend,
    }
}

/// A plugin tool's own description, with the plugin named so an agent
/// reading `tools/list` can tell where the tool came from.
fn plugin_tool_description(
    plugin: &LoadedPlugin,
    tool: &orbit_tools::plugin::ResolvedPluginTool,
) -> String {
    let attribution = format!(
        "Provided by the '{}' plugin v{}.",
        plugin.namespace(),
        plugin.manifest.metadata.version
    );
    if tool.description.trim().is_empty() {
        attribution
    } else {
        format!("{} {attribution}", tool.description.trim())
    }
}

/// `requires.orbit` / `requires.host_api` / `requires.platforms` against this
/// binary and machine. `None` means every requirement holds.
pub fn unmet_requirement(plugin: &LoadedPlugin) -> Option<String> {
    let requires = &plugin.manifest.spec.requires;
    if let Some(host_api) = requires.host_api
        && host_api != PLUGIN_HOST_API
    {
        return Some(format!(
            "plugin '{}' requires host_api {host_api}; this Orbit speaks {PLUGIN_HOST_API}. \
             Install a build of the plugin for this host API.",
            plugin.namespace()
        ));
    }
    if let Some(range) = &requires.orbit {
        let host_version = host_version();
        match SemverRange::parse(range) {
            Ok(range) if !range.matches(&host_version) => {
                return Some(format!(
                    "plugin '{}' requires orbit {range}; this host is {host_version}. Upgrade \
                     Orbit or install a plugin version that supports it.",
                    plugin.namespace()
                ));
            }
            Ok(_) => {}
            Err(error) => {
                return Some(format!(
                    "plugin '{}' declares an unreadable `requires.orbit`: {error}",
                    plugin.namespace()
                ));
            }
        }
    }
    let platform = current_platform();
    if !requires.platforms.is_empty()
        && !requires
            .platforms
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(platform))
    {
        return Some(format!(
            "plugin '{}' supports {} only; this machine is {platform}.",
            plugin.namespace(),
            requires.platforms.join(", ")
        ));
    }
    None
}

/// This binary's version, as `requires.orbit` compares against it.
pub fn host_version() -> Version {
    env!("CARGO_PKG_VERSION")
        .parse()
        .unwrap_or_else(|_| Version::new(0, 0, 0))
}

fn current_platform() -> &'static str {
    std::env::consts::OS
}
