//! Plugin pages: the page a dashboard frames and the tool calls it makes
//! (plugin-pages design `docs/design/plugin-pages/2_design.md` §2–§3).
//!
//! A page is untrusted code. Two things here keep it inside its plugin's
//! read-only surface: only a page that is active is handed out (its plugin
//! active in this workspace and its files hashing to the digest recorded at
//! install), and a call runs only a `read_only` tool of the same plugin that
//! the page declares, with input that fits the tool's schema. Everything is
//! checked before anything runs.

use std::sync::Arc;

use orbit_common::{NotFoundKind, OrbitError};
use orbit_tools::plugin::{
    CompiledSchema, LoadedPlugin, LoadedPluginPage, PluginPageBundle, page_digest_diagnostic,
};
use orbit_types::plugin::{PluginExecutionKind, plugin_tool_name};
use orbit_types::tool::{McpCapability, ToolSessionContext};
use serde_json::Value;

use crate::application::contracts::ToolInvocationPort;
use crate::{Core, OrbitRuntime};

/// An active page's files, held in the plugin load that hashed them, so the
/// bytes a caller writes out are the bytes that were hashed.
pub struct PageSource {
    plugin: Arc<LoadedPlugin>,
    index: usize,
}

impl PageSource {
    pub fn namespace(&self) -> &str {
        self.plugin.namespace()
    }

    pub fn id(&self) -> &str {
        &self.page().id
    }

    /// The bridge version the page was written for, when it names one.
    pub fn kit(&self) -> Option<u32> {
        self.page().kit
    }

    /// SHA-256 over the page's files, as recorded at install.
    pub fn digest(&self) -> &str {
        &self.bundle().digest
    }

    /// The bytes of `index.html`.
    pub fn markup(&self) -> &[u8] {
        &self.bundle().markup
    }

    /// The listed scripts, in manifest order, as `(name, bytes)`.
    pub fn scripts(&self) -> impl Iterator<Item = (&str, &[u8])> {
        self.bundle()
            .scripts
            .iter()
            .map(|file| (file.name.as_str(), file.bytes.as_slice()))
    }

    /// The listed styles, in manifest order, as `(name, bytes)`.
    pub fn styles(&self) -> impl Iterator<Item = (&str, &[u8])> {
        self.bundle()
            .styles
            .iter()
            .map(|file| (file.name.as_str(), file.bytes.as_slice()))
    }

    fn page(&self) -> &LoadedPluginPage {
        &self.plugin.pages[self.index]
    }

    fn bundle(&self) -> &PluginPageBundle {
        match &self.page().bundle {
            Ok(bundle) => bundle,
            // `resolve_page` hands out only a page whose bundle read.
            Err(_) => unreachable!("an active page always holds its bundle"),
        }
    }
}

/// One checked page call. Nothing has run yet: the dashboard runs it on a
/// miss of its in-flight sharing, so identical calls share one execution.
pub struct PageCall {
    runtime: OrbitRuntime,
    tool: String,
    input: Value,
}

impl PageCall {
    /// Canonical name of the tool this call runs.
    pub fn tool(&self) -> &str {
        &self.tool
    }

    /// The input the call passes, after its schema check.
    pub fn input(&self) -> &Value {
        &self.input
    }

    /// Run the tool through the audited dispatch a panel read uses.
    pub fn run(self) -> Result<Value, OrbitError> {
        self.runtime.run_tool_command_with_session(
            &self.tool,
            self.input,
            None,
            None,
            ToolSessionContext {
                effective_capabilities: [McpCapability::Agent].into_iter().collect(),
                ..ToolSessionContext::default()
            },
        )
    }
}

/// The active page `page_id` of `namespace`, or a not-found naming why it is
/// not served: no such plugin, a plugin that is not active in this
/// workspace, no such page, or a page whose files fail their checks or no
/// longer hash to the digest recorded at install.
pub(super) fn resolve_page(
    core: &Core,
    namespace: &str,
    page_id: &str,
) -> Result<PageSource, OrbitError> {
    let runtime = core.runtime();
    let not_found = |message: String| OrbitError::not_found(NotFoundKind::Tool, message);
    let load = runtime.plugin_load();
    if load.is_disabled_in_workspace(namespace) {
        return Err(not_found(format!(
            "plugin '{namespace}' is disabled in this workspace, so its pages are not served"
        )));
    }
    let plugin = load
        .active()
        .find(|plugin| plugin.namespace() == namespace)
        .ok_or_else(|| not_found(format!("plugin '{namespace}' is not active on this host")))?;
    let index = plugin
        .pages
        .iter()
        .position(|page| page.id == page_id)
        .ok_or_else(|| not_found(format!("plugin '{namespace}' declares no page '{page_id}'")))?;
    let recorded = load
        .installed
        .iter()
        .find(|installed| installed.name == namespace)
        .and_then(|installed| installed.page_digests.get(page_id))
        .map(String::as_str);
    if let Some(diagnostic) = page_digest_diagnostic(namespace, recorded, &plugin.pages[index]) {
        return Err(not_found(diagnostic));
    }
    Ok(PageSource {
        plugin: Arc::clone(plugin),
        index,
    })
}

/// Check one call from an active page: the tool is this plugin's, exists,
/// is `read_only`, is one the page declares, and `input` fits its schema.
/// A tool the page may not call is a policy refusal and input that does not
/// fit is invalid input; a page that is not served is a not-found.
pub(super) fn check_page_call(
    core: &Core,
    namespace: &str,
    page_id: &str,
    tool: &str,
    input: Value,
) -> Result<PageCall, OrbitError> {
    let source = resolve_page(core, namespace, page_id)?;
    let plugin = &source.plugin;
    let first_party = plugin.manifest.claims_first_party_namespace();
    let prefix = plugin_tool_name(namespace, "", first_party);
    // The bridge names a verb; a canonical name of this plugin's tool is
    // accepted too. Any other dotted name is another plugin's.
    let verb = match tool.strip_prefix(prefix.as_str()) {
        Some(verb) => verb,
        None if tool.contains('.') => {
            return Err(OrbitError::PolicyDenied(format!(
                "tool '{tool}' is not a tool of plugin '{namespace}'; a page may call only \
                     its own plugin's tools"
            )));
        }
        None => tool,
    };
    let Some(resolved) = plugin.tools.iter().find(|candidate| candidate.verb == verb) else {
        return Err(OrbitError::PolicyDenied(format!(
            "plugin '{namespace}' has no tool '{verb}'"
        )));
    };
    if resolved.execution_kind != PluginExecutionKind::ReadOnly {
        return Err(OrbitError::PolicyDenied(format!(
            "tool '{verb}' of plugin '{namespace}' may change state; a page may call only \
                 `read_only` tools"
        )));
    }
    if !source.page().tools.iter().any(|declared| declared == verb) {
        return Err(OrbitError::PolicyDenied(format!(
            "page '{page_id}' of plugin '{namespace}' does not declare tool '{verb}'; list \
                 it under the page's `tools`"
        )));
    }
    let schema = CompiledSchema::compile(resolved.input_schema.clone()).map_err(|error| {
        OrbitError::Execution(format!(
            "input schema of tool '{verb}' of plugin '{namespace}': {error}"
        ))
    })?;
    if let Some(violations) = schema.violations(&input) {
        return Err(OrbitError::InvalidInput(format!(
            "input for tool '{verb}' does not fit its schema: {violations}"
        )));
    }
    Ok(PageCall {
        runtime: core.runtime().clone(),
        tool: plugin_tool_name(namespace, verb, first_party),
        input,
    })
}
