//! Dashboard panels and link tiles (design §4.7).
//!
//! A panel is one `read_only` tool's JSON drawn by the dashboard's generic
//! renderer. Two things keep that safe to serve to any dashboard session:
//! the manifest may only declare a panel over a `read_only` tool
//! (`PluginManifest::validate_structure` refuses anything else, so a
//! mutating source never reaches an installed plugin), and a read here
//! executes only a tool a panel names, with no caller-supplied input.

use std::collections::BTreeMap;

use orbit_common::OrbitError;
use orbit_tools::plugin::LoadedPlugin;
use orbit_types::plugin::{
    DEFAULT_PANEL_REFRESH_MS, PluginExecutionKind, PluginPanelGroup, PluginPanelRender,
    PluginTemplateVars, PluginWebPanel, plugin_tool_name, render_template, template_references,
};
use orbit_types::tool::{McpCapability, ToolSessionContext};
use serde_json::Value;

use crate::OrbitRuntime;

/// One declared panel, as the API reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginPanelSummary {
    pub id: String,
    pub title: String,
    /// Canonical name of the `read_only` tool this panel reads.
    pub tool: String,
    pub render: PluginPanelRender,
    pub group: PluginPanelGroup,
    /// Effective server-side cache window for this panel.
    pub refresh_ms: u64,
}

/// One link tile, with its `{{config.<key>}}` references resolved against
/// the plugin's effective configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginLinkSummary {
    pub title: String,
    pub url: String,
}

/// Project an active plugin's `spec.web` section.
pub(super) fn web_summaries(
    plugin: &LoadedPlugin,
    first_party: bool,
    config_values: &BTreeMap<String, String>,
) -> (Vec<PluginPanelSummary>, Vec<PluginLinkSummary>) {
    let Some(web) = plugin.manifest.spec.web.as_ref() else {
        return (Vec::new(), Vec::new());
    };
    let panels = web
        .panels
        .iter()
        .filter_map(|panel| {
            let verb = panel.source_verb()?;
            Some(PluginPanelSummary {
                id: panel.id.clone(),
                title: if panel.title.trim().is_empty() {
                    panel.id.clone()
                } else {
                    panel.title.clone()
                },
                tool: plugin_tool_name(plugin.namespace(), verb, first_party),
                render: panel.render,
                group: panel.group,
                refresh_ms: panel.refresh_ms.unwrap_or(DEFAULT_PANEL_REFRESH_MS),
            })
        })
        .collect();
    let vars = PluginTemplateVars {
        workspace: None,
        plugin_root: plugin.root.to_string_lossy().into_owned(),
        plugin_state: String::new(),
        config: config_values.clone(),
    };
    let links = web
        .links
        .iter()
        .enumerate()
        .map(|(index, link)| PluginLinkSummary {
            title: link.title.clone(),
            // A reference the configuration does not answer leaves the
            // template visible rather than half-rendering a URL: an operator
            // seeing `{{config.port}}` knows which key to set.
            url: if template_references(&link.url)
                .iter()
                .any(|reference| matches!(reference.as_str(), "workspace" | "plugin_state"))
            {
                // Dashboard summaries have no backend invocation context.
                // Keep the complete template visible instead of partially
                // resolving other references around an unavailable one.
                link.url.clone()
            } else {
                render_template(&link.url, &vars, &format!("spec.web.links[{index}].url"))
                    .unwrap_or_else(|_| link.url.clone())
            },
        })
        .collect();
    (panels, links)
}

/// Read one panel: execute its `read_only` source through the same audited
/// dispatch `orbit tool run` uses, and return the tool's JSON.
///
/// The dashboard is loopback-bound and the governed row for a `read_only`
/// plugin tool (`plugin.tool.read_only`) admits every caller Orbit can name,
/// so a panel read names the session as an agent rather than requiring the
/// operator session the dashboard's *mutations* require.
pub fn read_plugin_panel(
    runtime: &OrbitRuntime,
    namespace: &str,
    panel_id: &str,
) -> Result<Value, OrbitError> {
    let (plugin, panel) = plugin_panel(runtime, namespace, panel_id)?;
    let verb = panel.source_verb().ok_or_else(|| {
        OrbitError::InvalidInput(format!(
            "panel '{panel_id}' of plugin '{namespace}' has no `tool:<verb>` source"
        ))
    })?;
    let resolved = plugin
        .tools
        .iter()
        .find(|tool| tool.verb == verb)
        .ok_or_else(|| {
            OrbitError::InvalidInput(format!(
                "panel '{panel_id}' of plugin '{namespace}' sources tool '{verb}', which the \
                 loaded manifest does not declare"
            ))
        })?;
    // The manifest could not have declared this panel over a mutating tool,
    // and the loaded manifest is the one the tool surface was built from.
    // Re-checking here costs nothing and keeps the refusal local to the
    // request that would have run it.
    if resolved.execution_kind != PluginExecutionKind::ReadOnly {
        return Err(OrbitError::InvalidInput(format!(
            "panel '{panel_id}' of plugin '{namespace}' sources the mutating tool '{verb}'; a \
             dashboard panel may only read a `read_only` tool"
        )));
    }
    let tool_name = plugin_tool_name(
        namespace,
        verb,
        plugin.manifest.claims_first_party_namespace(),
    );
    runtime.execute_tool_command_with_session_context(
        &tool_name,
        Value::Object(Default::default()),
        None,
        None,
        ToolSessionContext {
            effective_capabilities: [McpCapability::Agent].into_iter().collect(),
            ..ToolSessionContext::default()
        },
    )
}

/// Effective cache window for one declared panel.
pub fn plugin_panel_refresh_ms(
    runtime: &OrbitRuntime,
    namespace: &str,
    panel_id: &str,
) -> Result<u64, OrbitError> {
    let (_, panel) = plugin_panel(runtime, namespace, panel_id)?;
    Ok(panel.refresh_ms.unwrap_or(DEFAULT_PANEL_REFRESH_MS))
}

fn plugin_panel<'a>(
    runtime: &'a OrbitRuntime,
    namespace: &str,
    panel_id: &str,
) -> Result<(&'a LoadedPlugin, &'a PluginWebPanel), OrbitError> {
    let plugin = runtime
        .plugin_load()
        .active()
        .find(|plugin| plugin.namespace() == namespace)
        .ok_or_else(|| {
            OrbitError::not_found(
                orbit_common::NotFoundKind::Tool,
                format!("plugin '{namespace}' is not active on this host"),
            )
        })?;
    let panel = plugin
        .manifest
        .spec
        .web
        .as_ref()
        .and_then(|web| web.panels.iter().find(|panel| panel.id == panel_id))
        .ok_or_else(|| {
            OrbitError::not_found(
                orbit_common::NotFoundKind::Tool,
                format!("plugin '{namespace}' declares no panel '{panel_id}'"),
            )
        })?;
    Ok((plugin, panel))
}
