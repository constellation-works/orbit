//! Read-only plugin discovery without a runtime: host-global MCP tools and
//! CLI groups, plus plugin-only registries with optional workspace toggles.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

use orbit_common::OrbitError;
use orbit_store::Store;
use orbit_tools::ToolRegistry;
use orbit_types::plugin::{PluginStatus, plugin_tool_name};
use orbit_types::tool::McpToolDefinition;

use super::host::{self, PluginHostLoad};

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
    let load = host::load_workspace_plugins_without_refusal_audit(
        global_root,
        global_root,
        &store,
        &mut registry,
        plugin_config,
        &BTreeMap::new(),
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
    // No workspace here, so no pin file: the global root holds none, and an
    // absent pin file is a valid configuration.
    workspace_plugin_registry(
        global_root,
        global_root,
        audit_db,
        plugin_config,
        &BTreeMap::new(),
    )
}

/// A plugin-only registry for one workspace, computed without writes.
///
/// Uses the runtime's trust and definition checks with the caller's resolved
/// plugin config and workspace toggles. Disabled plugins are excluded before
/// resolving contribution collisions, so they cannot reserve catalog names.
/// Refused rows are reported without writing refusal-audit events.
pub fn workspace_plugin_registry(
    global_root: &Path,
    workspace_orbit_root: &Path,
    audit_db: &Path,
    plugin_config: &BTreeMap<String, Value>,
    workspace_toggles: &BTreeMap<String, bool>,
) -> Result<(ToolRegistry, PluginHostLoad), OrbitError> {
    let store = Store::open_read_only(audit_db)?;
    let mut registry = ToolRegistry::new();
    let load = host::load_workspace_plugins_without_refusal_audit(
        global_root,
        workspace_orbit_root,
        &store,
        &mut registry,
        plugin_config,
        workspace_toggles,
    );
    Ok((registry, load))
}
