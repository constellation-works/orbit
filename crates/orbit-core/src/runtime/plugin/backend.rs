//! The backend a plugin's tools share, and the registry tools built on it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;

use orbit_tools::plugin::{
    LoadedPlugin, McpBackend, McpExpectedTool, PluginBackend, PluginBackendSpec,
    PluginConfigSection, PluginTool, PluginToolBinding,
};
use orbit_types::plugin::{
    InstalledPlugin, PluginBackendType, PluginGrantSet, PluginProvenance, parse_stored_grants,
};

use super::grants::recorded_program_paths;
use super::paths::plugin_state_dir;

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
    let grants = parse_stored_grants(&installed.grants).unwrap_or_default();
    // `{{config.<key>}}` resolves against the effective section: what the
    // operator configured in `[plugins.<ns>]`, over what the manifest
    // defaults (§1).
    let config = super::config::plugin_config_section(plugin, plugin_config);
    build_plugin_backend(
        plugin,
        PluginProvenance {
            name: installed.name.clone(),
            version: installed.version.clone(),
            manifest_digest: plugin.manifest_digest.clone(),
            // The audit row carries the set as recorded, scopes included:
            // "ran with `fs`" and "ran with `fs` narrowed to one directory"
            // are different facts about the same call (design §4.4).
            grants: grants.to_recorded(),
        },
        &plugin_state_dir(global_root, &installed.name),
        global_root,
        grants,
        config,
        // The paths the enabling command resolved, never this caller's
        // `PATH`: which process spawns the backend must not change what it
        // may execute (design §4.3).
        recorded_program_paths(global_root, &installed.name),
    )
}

/// Construct the backend shared by runtime registration and conformance.
pub(crate) fn build_plugin_backend(
    plugin: &LoadedPlugin,
    provenance: PluginProvenance,
    state_dir: &Path,
    global_root: &Path,
    grants: PluginGrantSet,
    config: PluginConfigSection,
    program_paths: BTreeMap<String, PathBuf>,
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
        program_paths,
        config,
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

pub(super) fn plugin_tool(
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
