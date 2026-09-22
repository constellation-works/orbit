//! Register installed plugins into the runtime tool registry.
//!
//! Fail closed per plugin (design `docs/design/plugins/1_scope.md` §4.9): a
//! plugin whose manifest no longer loads, whose `requires` no longer hold,
//! whose namespace collides, whose required grants the operator has not
//! recorded, or whose `plugins` row this host cannot verify is reported as one
//! diagnostic and registered inactive; every built-in and every other plugin is
//! untouched.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;

use orbit_common::OrbitError;
use orbit_common::observability::audit_id::audit_execution_id;
use orbit_store::Store;
use orbit_store::contracts::{AuditEventInsertParams, AuditInvocationFields};
use orbit_tools::ToolRegistry;
use orbit_tools::plugin::{
    LoadedPlugin, McpBackend, McpExpectedTool, PluginBackend, PluginBackendSpec, PluginTool,
    PluginToolBinding, PluginValidationPolicy, load_plugin_dir, refuse_covering_fs_write_roots,
    validate_loaded_plugin,
};
use orbit_types::plugin::{
    InstalledPlugin, PLUGIN_HOST_API, PluginBackendType, PluginGrant, PluginMcpScope,
    PluginPinFile, PluginProvenance, PluginStatus, SemverRange, Version, parse_stored_grants,
    plugin_tool_name,
};
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::tool::{McpToolDefinition, McpToolScope};

use super::plugin_grants::{verify_install_path, verify_recorded_grants};

/// Why a plugin is not on the active tool surface, and what would fix it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginDiagnostic {
    pub plugin: String,
    pub status: PluginStatus,
    pub message: String,
}

/// The status a loaded plugin will have on the next host load.
///
/// Lifecycle commands use this before reporting their result, and the loader
/// uses it before registering tools, so both surfaces apply one eligibility
/// decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProjectedPluginStatus {
    pub(crate) status: PluginStatus,
    pub(crate) diagnostic: Option<String>,
    register_inactive_tools: bool,
}

impl ProjectedPluginStatus {
    fn active() -> Self {
        Self {
            status: PluginStatus::Active,
            diagnostic: None,
            register_inactive_tools: false,
        }
    }

    fn inactive(message: String, register_inactive_tools: bool) -> Self {
        Self {
            status: PluginStatus::Inactive,
            diagnostic: Some(message),
            register_inactive_tools,
        }
    }
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
    /// Whether the loader could verify this row's grants against the set
    /// `orbit plugin enable` authorized [ORB-12778]. `false` means the row
    /// granted nothing, whatever it claims, so no surface may present its
    /// grants as effective.
    pub grants_authorized: bool,
    /// The manifest this pass loaded, when it loaded at all. Retained so the
    /// catalog layer, the seeded definitions and the config contract all read
    /// the same document the tool surface was built from.
    pub loaded: Option<Arc<LoadedPlugin>>,
    /// `[plugins.<ns>]` over the manifest's defaults, as the backend and the
    /// manifest's `{{config.<key>}}` templates see it. Retained so a dashboard
    /// link tile renders the same value the plugin itself runs with (§4.7).
    pub config_values: BTreeMap<String, String>,
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

/// The one directory this host installs every version of `name` into.
///
/// Trusted layout: it is derived from the namespace and the global root, never
/// from the `plugins` row, so a lifecycle verb can clean up after a row whose
/// recorded `install_path` it refuses to touch [ORB-12800].
pub fn plugin_namespace_dir(global_root: &Path, name: &str) -> PathBuf {
    plugin_install_root(global_root).join(name)
}

pub fn plugin_install_path(global_root: &Path, name: &str, version: &str) -> PathBuf {
    plugin_namespace_dir(global_root, name).join(version)
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
///
/// `plugin_config` is the caller's resolved global-only `[plugins.<ns>]`
/// sections (config layered over itself, no workspace); without it a plugin
/// whose config schema requires a key only `config.toml` sets is refused
/// here even though the workspace runtime loads it fine.
pub fn host_plugin_mcp_definitions(
    global_root: &Path,
    audit_db: &Path,
    plugin_config: &BTreeMap<String, Value>,
) -> Result<Vec<McpToolDefinition>, OrbitError> {
    let registry = host_plugin_registry(global_root, audit_db, plugin_config)?.0;
    registry
        .mcp_tool_definitions()
        .map_err(|error| OrbitError::InvalidInput(error.to_string()))
}

/// One `orbit <ns>` command group, derived from an active plugin's manifest
/// (design §4.6).
///
/// Only an **active** plugin contributes a group: a disabled or refused
/// plugin has no `orbit <ns>` at all, so `orbit <ns>` is the ordinary
/// unknown-command error rather than a group whose every call fails.
#[derive(Debug, Clone, PartialEq)]
pub struct PluginCliGroup {
    pub namespace: String,
    pub version: String,
    pub description: String,
    pub verbs: Vec<PluginCliVerb>,
}

/// One `orbit <ns> <verb>`: the tool it dispatches to and the schema its
/// flags are derived from.
#[derive(Debug, Clone, PartialEq)]
pub struct PluginCliVerb {
    /// The subcommand name: `cli.verb` when the manifest overrides it.
    pub verb: String,
    /// Canonical registry name this subcommand dispatches to.
    pub tool_name: String,
    pub description: String,
    /// The tool's resolved `input_schema`.
    pub input_schema: Value,
    /// `cli.positional`, in manifest order.
    pub positional: Vec<String>,
    pub mutating: bool,
}

/// Every `orbit <ns>` group this host serves.
///
/// Read host-globally and without a workspace runtime, because the CLI
/// builds its clap tree before it bootstraps one. A host with no enabled
/// plugin answers without touching a manifest.
///
/// `plugin_config` is the caller's resolved global-only `[plugins.<ns>]`
/// sections (config layered over itself, no workspace); without it a plugin
/// whose config schema requires a key only `config.toml` sets is refused
/// here even though the workspace runtime loads it fine.
pub fn host_plugin_cli_groups(
    global_root: &Path,
    audit_db: &Path,
    plugin_config: &BTreeMap<String, Value>,
) -> Result<Vec<PluginCliGroup>, OrbitError> {
    let store = Store::open_read_only(audit_db)?;
    if !store
        .list_plugins()?
        .iter()
        .any(|installed| installed.enabled)
    {
        return Ok(Vec::new());
    }
    let mut registry = ToolRegistry::new();
    let load = load_host_plugins(
        global_root,
        global_root,
        &store,
        &mut registry,
        plugin_config,
    );
    Ok(plugin_cli_groups(&load))
}

/// Project one load pass into its CLI groups.
pub fn plugin_cli_groups(load: &PluginHostLoad) -> Vec<PluginCliGroup> {
    let mut groups: Vec<PluginCliGroup> = load
        .registered
        .iter()
        .filter(|entry| entry.status == PluginStatus::Active)
        .filter_map(|entry| {
            let plugin = entry.loaded.as_ref()?;
            let first_party = plugin.manifest.claims_first_party_namespace();
            let verbs = plugin
                .tools
                .iter()
                .map(|tool| {
                    let shape = plugin
                        .manifest
                        .spec
                        .tools
                        .iter()
                        .find(|declared| declared.name == tool.verb)
                        .and_then(|declared| declared.cli.as_ref());
                    PluginCliVerb {
                        verb: shape
                            .and_then(|shape| shape.verb.clone())
                            .unwrap_or_else(|| tool.verb.clone()),
                        tool_name: plugin_tool_name(plugin.namespace(), &tool.verb, first_party),
                        description: tool.description.clone(),
                        input_schema: tool.input_schema.clone(),
                        positional: shape
                            .map(|shape| shape.positional.clone())
                            .unwrap_or_default(),
                        mutating: tool.execution_kind
                            == orbit_types::plugin::PluginExecutionKind::Mutating,
                    }
                })
                .collect();
            Some(PluginCliGroup {
                namespace: plugin.namespace().to_string(),
                version: plugin.manifest.metadata.version.clone(),
                description: plugin.manifest.metadata.description.clone(),
                verbs,
            })
        })
        .collect();
    groups.sort_by(|left, right| left.namespace.cmp(&right.namespace));
    groups
}

/// A registry holding this host's plugin tools and nothing else.
///
/// Namespace validation still runs against the real built-in names, so a
/// colliding plugin is refused here exactly as it is in a workspace runtime.
///
/// `plugin_config` is the caller's resolved global-only `[plugins.<ns>]`
/// sections (config layered over itself, no workspace); without it a plugin
/// whose config schema requires a key only `config.toml` sets is refused
/// here even though the workspace runtime loads it fine, and any
/// `mcp_scope: global` tool this registry executes renders
/// `{{config.<key>}}` from manifest defaults only.
pub fn host_plugin_registry(
    global_root: &Path,
    audit_db: &Path,
    plugin_config: &BTreeMap<String, Value>,
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
        plugin_config,
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
    for plugin in &installed {
        // The grant set a row records is only authority when `orbit plugin
        // enable` wrote it. A backend that can write `orbit.db` can write its
        // own row, so an enabled row is checked against the authorization
        // witness before anything it claims is honoured [ORB-12778]. The
        // witness does not cover `install_path`, so the path is checked
        // structurally beside it: a row that keeps its authorized grant names
        // but points them at a tree outside `plugins/<ns>/` is a tree the
        // backend could have written itself, and nothing is read from it
        // [ORB-12785].
        if plugin.enabled
            && let Err((check, message)) = verify_enabled_row(global_root, plugin)
        {
            audit_refused_row(store, plugin, check, &message);
            load.diagnostics.push(PluginDiagnostic {
                plugin: plugin.name.clone(),
                status: PluginStatus::Inactive,
                message: message.clone(),
            });
            // No tools at all, not even inactive ones: an inactive entry is
            // how the host explains a plugin it trusts but cannot serve, and
            // this row is one it does not trust.
            load.registered.push(RegisteredPlugin {
                name: plugin.name.clone(),
                version: plugin.version.clone(),
                status: PluginStatus::Inactive,
                tools: Vec::new(),
                diagnostic: Some(message),
                grants_authorized: false,
                loaded: None,
                config_values: BTreeMap::new(),
            });
            continue;
        }
        let registered = register_installed_plugin(global_root, plugin, registry, plugin_config);
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

/// The row checks that run before anything the row names is read: the grant
/// witness [ORB-12778], then the install path [ORB-12785]. `Err` carries the
/// audit subcommand naming the check that refused, and its diagnostic.
fn verify_enabled_row(
    global_root: &Path,
    installed: &InstalledPlugin,
) -> Result<(), (&'static str, String)> {
    verify_recorded_grants(global_root, installed).map_err(|message| ("verify_grants", message))?;
    verify_install_path(global_root, installed)
        .map_err(|message| ("verify_install_path", message))?;
    Ok(())
}

/// Write the refusal of a `plugins` row — an unauthorized grant set
/// [ORB-12778] or a relocated install [ORB-12785] — to the audit trail.
///
/// The load pass is the only place this is visible, and a refusal that left no
/// durable record would be indistinguishable from a plugin the operator had
/// disabled. One row per load pass: the trail then says how often the
/// tampered row was presented, not just that it exists once.
///
/// A failed write is logged and swallowed, like every other audit write on a
/// path that is already refusing (`record_authorization_event`): the plugin is
/// not registered either way, and `host_plugin_registry` deliberately opens the
/// store read-only, so an insert failure here is an expected outcome rather
/// than a new one to propagate.
fn audit_refused_row(store: &Store, installed: &InstalledPlugin, check: &str, message: &str) {
    let params = AuditEventInsertParams {
        execution_id: audit_execution_id("plugin-load"),
        command: "plugin.load".to_string(),
        subcommand: Some(check.to_string()),
        tool_name: None,
        target_type: Some("plugin".to_string()),
        target_id: Some(installed.name.clone()),
        role: "admin".to_string(),
        status: AuditEventStatus::Denied,
        exit_code: 1,
        duration_ms: 0,
        working_directory: std::env::current_dir()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|_| ".".to_string()),
        // The claimed set and path, not the authorized ones: what the row
        // asked this host to honour is the fact an operator investigating needs.
        arguments_json: Some(
            serde_json::json!({
                "plugin": installed.name,
                "version": installed.version,
                "claimed_grants": installed.grants,
                "install_path": installed.install_path,
            })
            .to_string(),
        ),
        stdout_truncated: None,
        stderr_truncated: None,
        error_message: Some(message.to_string()),
        host: std::env::var("HOSTNAME").ok(),
        pid: std::process::id(),
        session_id: None,
        workspace_id: None,
        caller_machine_id: None,
        caller_machine_name: None,
        process_machine_id: None,
        process_machine_name: None,
        transport: None,
        effective_capabilities: Default::default(),
        origin_session_id: None,
        mcp_call_id: None,
        lease_id: None,
        task_id: None,
        job_run_id: None,
        activity_id: None,
        step_index: None,
    };
    // The dedicated plugin columns carry the identity, so `orbit audit` reads
    // this row beside the plugin's tool calls. Its grant set is empty on
    // purpose: those columns record what a plugin *ran under*, and this one
    // ran nothing — the set it claimed is in `arguments_json` above.
    let provenance = PluginProvenance {
        name: installed.name.clone(),
        version: installed.version.clone(),
        manifest_digest: installed.manifest_digest.clone(),
        grants: Vec::new(),
    };
    let invocation = AuditInvocationFields {
        plugin: Some(&provenance),
        ..Default::default()
    };
    if let Err(error) = store.insert_audit_event_record_with_invocation(&params, invocation) {
        tracing::error!(
            target: "orbit.core.plugin",
            plugin = %installed.name,
            check,
            "could not audit the refused plugin row: {error}",
        );
    }
}

fn register_installed_plugin(
    global_root: &Path,
    installed: &InstalledPlugin,
    registry: &mut ToolRegistry,
    plugin_config: &BTreeMap<String, Value>,
) -> RegisteredPlugin {
    let refused = |status: PluginStatus, message: String| RegisteredPlugin {
        name: installed.name.clone(),
        version: installed.version.clone(),
        status,
        tools: Vec::new(),
        diagnostic: Some(message),
        grants_authorized: true,
        loaded: None,
        config_values: BTreeMap::new(),
    };

    if !installed.enabled {
        return RegisteredPlugin {
            name: installed.name.clone(),
            version: installed.version.clone(),
            status: PluginStatus::Disabled,
            tools: Vec::new(),
            diagnostic: None,
            grants_authorized: true,
            loaded: None,
            config_values: BTreeMap::new(),
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
    let projection = projected_status(installed, &plugin, global_root, plugin_config);
    if projection.status == PluginStatus::Inactive {
        let message = projection
            .diagnostic
            .unwrap_or_else(|| format!("plugin '{}' is inactive", installed.name));
        if projection.register_inactive_tools {
            return register_inactive_tools(global_root, installed, &plugin, registry, message);
        }
        return refused(PluginStatus::Inactive, message);
    }

    let backend = plugin_backend(global_root, installed, &plugin, plugin_config);
    let provenance = backend.spec().provenance.clone();
    let mut tools = Vec::with_capacity(plugin.tools.len());
    for tool in &plugin.tools {
        let name = registered_plugin_tool_name(&plugin, &tool.verb);
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
        grants_authorized: true,
        config_values: backend.spec().config_values.clone(),
        loaded: Some(Arc::new(plugin)),
    }
}

/// Derive the status the host loader will assign to a loaded plugin without
/// registering any tools or mutating host state.
pub(crate) fn projected_status(
    installed: &InstalledPlugin,
    plugin: &LoadedPlugin,
    global_root: &Path,
    plugin_config: &BTreeMap<String, Value>,
) -> ProjectedPluginStatus {
    if let Some(message) = first_party_row_mismatch(installed, plugin) {
        return ProjectedPluginStatus::inactive(message, false);
    }
    // Runs before anything below reads `installed.grants` through
    // `plugin_backend`'s own parse: a name it does not recognize must refuse
    // the row here, not fall silently out of a `filter_map` there.
    if let Some(message) = unknown_grant_diagnostic(installed) {
        return ProjectedPluginStatus::inactive(message, true);
    }
    // Validate the manifest actually on disk before deciding what a digest
    // mismatch means: an on-disk edit that also breaks the namespace rules
    // (§4.9) is a plain refusal, not tool names inserted as inactive first.
    let policy =
        PluginValidationPolicy::host_default().with_first_party_verified(installed.first_party);
    if let Err(error) = validate_loaded_plugin(plugin, &policy) {
        return ProjectedPluginStatus::inactive(
            format!("plugin '{}' is refused: {error}", installed.name),
            false,
        );
    }
    if plugin.manifest_digest != installed.manifest_digest {
        return ProjectedPluginStatus::inactive(
            digest_mismatch_diagnostic(installed, plugin),
            true,
        );
    }
    let backend = plugin_backend(global_root, installed, plugin, plugin_config);
    if let Err(error) = refuse_covering_fs_write_roots(backend.spec(), None) {
        return ProjectedPluginStatus::inactive(
            format!("plugin '{}' is refused: {error}", installed.name),
            true,
        );
    }
    if let Some(message) = unmet_requirement(plugin) {
        return ProjectedPluginStatus::inactive(message, true);
    }
    if let Some(message) = missing_grant_diagnostic(installed, plugin) {
        return ProjectedPluginStatus::inactive(message, true);
    }
    // A plugin whose shipped definitions break the §4.5 rules, or whose
    // `[plugins.<ns>]` section its own schema rejects, contributes nothing:
    // registering its tools while its catalog layer is unusable would leave
    // half a plugin on the surface.
    if let Err(message) = validate_plugin_contributions(plugin, plugin_config) {
        return ProjectedPluginStatus::inactive(
            format!("plugin '{}' is refused: {message}", installed.name),
            false,
        );
    }
    ProjectedPluginStatus::active()
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
        let name = registered_plugin_tool_name(plugin, &tool.verb);
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
        grants_authorized: true,
        loaded: None,
        config_values: BTreeMap::new(),
    }
}

/// Canonical name the validator already checked: the manifest claim, never
/// the store row's `first_party` flag. The row is only the verification
/// answer passed into `with_first_party_verified`.
fn registered_plugin_tool_name(plugin: &LoadedPlugin, verb: &str) -> String {
    plugin.tool_name(verb, plugin.manifest.claims_first_party_namespace())
}

/// A `plugins` row that claims `first_party` for a manifest that does not
/// declare `origin: orbit`. Validation named tools from the manifest; a
/// `true` row would otherwise register `orbit.<ns>.*` over a built-in.
fn first_party_row_mismatch(installed: &InstalledPlugin, plugin: &LoadedPlugin) -> Option<String> {
    if installed.first_party && !plugin.manifest.claims_first_party_namespace() {
        Some(format!(
            "plugin '{}' is refused: the plugins row claims first_party but the manifest does \
             not declare `origin: orbit`",
            installed.name
        ))
    } else {
        None
    }
}

/// A row's grants contain a name no current [`PluginGrant`] recognizes:
/// retired, renamed, or written by a newer Orbit. `plugin_backend` would
/// otherwise drop it with a silent `filter_map`, running the plugin under
/// fewer grants than the operator authorized without saying so; refusing the
/// row instead surfaces it, naming the grant it cannot parse.
fn unknown_grant_diagnostic(installed: &InstalledPlugin) -> Option<String> {
    parse_stored_grants(&installed.grants)
        .err()
        .map(|error| format!("plugin '{}' is refused: {error}", installed.name))
}

/// The on-disk manifest is not the one this host recorded at install; the
/// grants apply only to that stored digest (design §4.1).
fn digest_mismatch_diagnostic(installed: &InstalledPlugin, plugin: &LoadedPlugin) -> String {
    format!(
        "plugin '{}' on-disk manifest digest {} does not match the stored digest {}; grants \
         apply only to the stored manifest. Re-consent with `orbit plugin add --force` and \
         `orbit plugin enable {}`",
        installed.name, plugin.manifest_digest, installed.manifest_digest, installed.name
    )
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
pub(crate) fn plugin_backend(
    global_root: &Path,
    installed: &InstalledPlugin,
    plugin: &LoadedPlugin,
    plugin_config: &BTreeMap<String, Value>,
) -> PluginBackend {
    // A row reaching this point either parsed cleanly, or is on the
    // register-inactive-tools path where the grant set no longer matters
    // (`unknown_grant_diagnostic` already refused it); either way there is no
    // error to surface here.
    let grants: Vec<PluginGrant> = parse_stored_grants(&installed.grants)
        .unwrap_or_default()
        .into_iter()
        .collect();
    // `{{config.<key>}}` resolves against the effective section: what the
    // operator configured in `[plugins.<ns>]`, over what the manifest
    // defaults (§1).
    let config_values = super::plugin_config::plugin_config_values(plugin, plugin_config);
    build_plugin_backend(
        plugin,
        PluginProvenance {
            name: installed.name.clone(),
            version: installed.version.clone(),
            manifest_digest: plugin.manifest_digest.clone(),
            grants: grants
                .iter()
                .map(|grant| grant.as_str().to_string())
                .collect(),
        },
        &plugin_state_dir(global_root, &installed.name),
        global_root,
        grants,
        config_values,
    )
}

/// Construct the backend shared by runtime registration and conformance.
pub(crate) fn build_plugin_backend(
    plugin: &LoadedPlugin,
    provenance: PluginProvenance,
    state_dir: &Path,
    global_root: &Path,
    grants: Vec<PluginGrant>,
    config_values: BTreeMap<String, String>,
) -> PluginBackend {
    let spec = Arc::new(PluginBackendSpec {
        provenance,
        plugin_root: plugin.root.clone(),
        state_dir: state_dir.to_path_buf(),
        global_root: global_root.to_path_buf(),
        command: plugin.backend_command.clone(),
        args: plugin.manifest.spec.backend.args.clone(),
        timeout_ms: plugin.manifest.spec.backend.timeout_ms,
        sandbox: plugin.manifest.spec.backend.sandbox,
        permissions: plugin.manifest.spec.permissions.clone(),
        programs: plugin.manifest.spec.requires.programs.clone(),
        config_values,
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
