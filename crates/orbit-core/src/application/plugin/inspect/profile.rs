//! The rendered call-time profile and the requested-versus-granted rows.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_tools::ToolContext;
use orbit_tools::plugin::{LoadedPlugin, PluginBackend};
use orbit_types::plugin::{parse_stored_grants, plugin_tool_name};

use super::summary::PluginPermissionSummary;

use crate::OrbitRuntime;

/// The exact sandbox and environment projection a validated backend would
/// receive for the selected workspace on this host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginRenderedProfile {
    pub workspace: String,
    pub read: Vec<String>,
    pub read_denies: Vec<String>,
    pub write: Vec<String>,
    pub write_files: Vec<String>,
    pub network: String,
    pub unsandboxed: bool,
    pub environments: Vec<PluginRenderedEnvironment>,
}

/// One child environment. Exec backends have one per tool because
/// `ORBIT_TOOL_NAME` differs; an MCP backend has one shared environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginRenderedEnvironment {
    pub tool: Option<String>,
    pub variables: BTreeMap<String, String>,
}

pub(super) fn render_backend_profile(
    runtime: &OrbitRuntime,
    plugin: &LoadedPlugin,
    backend: &PluginBackend,
    workspace: &Path,
) -> Result<PluginRenderedProfile, OrbitError> {
    let workspace = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let workspace_text = workspace.to_string_lossy().into_owned();
    let profile = backend.spec().sandbox_profile(Some(&workspace))?;
    let context = ToolContext {
        cwd: Some(workspace_text.clone()),
        workspace_root: Some(workspace.clone()),
        proc_allowed_programs: plugin.manifest.spec.requires.programs.clone(),
        proc_spawn_environment: Some(runtime.execution_env_policy().agent_subprocess_env(&[])),
        ..ToolContext::default()
    };
    let environment_tools: Vec<Option<String>> = match backend {
        PluginBackend::Exec(_) => plugin
            .tools
            .iter()
            .map(|tool| {
                Some(plugin_tool_name(
                    plugin.namespace(),
                    &tool.verb,
                    plugin.manifest.claims_first_party_namespace(),
                ))
            })
            .collect(),
        PluginBackend::Mcp(_) => vec![None],
    };
    let environments = environment_tools
        .into_iter()
        .map(|tool| PluginRenderedEnvironment {
            variables: backend
                .spec()
                .child_environment(&context, &workspace_text, tool.as_deref())
                .into_iter()
                .collect(),
            tool,
        })
        .collect();
    let paths = |items: Vec<PathBuf>| {
        items
            .into_iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect()
    };
    let network = match profile.network {
        orbit_types::plugin::PluginNetworkPermission::None => "none",
        orbit_types::plugin::PluginNetworkPermission::Loopback => "loopback",
        orbit_types::plugin::PluginNetworkPermission::Any => "any",
    }
    .to_string();
    Ok(PluginRenderedProfile {
        workspace: workspace_text,
        read: paths(profile.read),
        read_denies: paths(profile.read_denies),
        write: paths(profile.write),
        write_files: paths(profile.write_files),
        network,
        unsandboxed: profile.unsandboxed,
        environments,
    })
}

/// Requested versus granted, one row per grant, for `orbit plugin show`.
///
/// A row this build cannot parse reports nothing as granted: the loader
/// refuses it for the same reason, and repeating an unreadable claim back as
/// authority is what [ORB-12778] closed.
pub(super) fn permission_rows(
    plugin: &LoadedPlugin,
    granted: &[String],
) -> Vec<PluginPermissionSummary> {
    let granted = parse_stored_grants(granted).unwrap_or_default();
    plugin
        .manifest
        .grant_requests()
        .into_iter()
        .map(|request| PluginPermissionSummary {
            granted: granted.contains(request.grant),
            granted_roots: granted
                .entry(request.grant)
                .and_then(|entry| entry.roots.clone()),
            grant: request.grant,
            requested: request.requested,
        })
        .collect()
}
