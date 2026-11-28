//! Plugin pages: the page a dashboard frames and the tool calls it makes
//! (plugin-pages design `docs/design/plugin-pages/2_design.md` §2–§3).
//!
//! A page is untrusted code. Two things here keep it inside its plugin's
//! read-only surface: only a page that is active is handed out (its plugin
//! active in this workspace and its files hashing to the digest recorded at
//! install), and a call runs only a `read_only` tool of the same plugin that
//! the page declares, with input that fits the tool's schema. An action — a
//! mutating tool the page names under `actions` — additionally needs the
//! operator session, the operator's consent to the action content, and a
//! one-time confirmation the dashboard obtained outside the frame for that
//! exact plugin, page, tool and input. Everything is checked before anything
//! runs.

use std::sync::Arc;

use orbit_common::governance::authorization::DASHBOARD_PLUGIN_PAGE_ACTION;
use orbit_common::{NotFoundKind, OrbitError};
use orbit_tools::plugin::{
    CompiledSchema, LoadedPlugin, LoadedPluginPage, PluginPageBundle, ResolvedPluginTool,
    page_digest_diagnostic,
};
use orbit_types::plugin::{PluginExecutionKind, plugin_tool_name};
use orbit_types::task::TaskWriteSurface;
use orbit_types::tool::{McpCapability, ToolSessionContext};
use serde_json::{Value, json};

use crate::app::exec::{DashboardOrigin, Operation};
use crate::application::contracts::ToolInvocationPort;
use crate::runtime::plugin::grants;
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

    fn first_party(&self) -> bool {
        self.plugin.manifest.claims_first_party_namespace()
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

/// One page action the server has checked and nothing has run for: what the
/// dashboard's confirmation names.
#[derive(Debug, Clone)]
pub struct PageAction {
    /// The plugin's namespace.
    pub plugin: String,
    /// The plugin's version.
    pub version: String,
    /// The page that asked.
    pub page: String,
    /// The page's sub-tab label.
    pub page_title: String,
    /// Canonical name of the mutating tool.
    pub tool: String,
    /// The tool's manifest description.
    pub description: String,
    /// The input the action passes, after its schema check.
    pub input: Value,
}

/// One action request a dashboard relays, with its server-side confirmation
/// verdict. The dashboard consumes the one-time confirmation before it asks,
/// so `confirmation` is `Err` with the refusal when it was missing, used,
/// expired or issued for another plugin, page, tool or input.
pub struct PageActionRun {
    pub namespace: String,
    pub page: String,
    pub tool: Option<String>,
    pub input: Value,
    pub confirmation: Result<(), String>,
    /// The web session the request arrived on, recorded on its audit row and
    /// on the tool call's.
    pub session: Option<String>,
    /// The serving host's name, recorded on the audit row.
    pub host: Option<String>,
}

impl PageActionRun {
    /// The governed operation an action requires, and its audit row, which
    /// is written for every request, refused or run.
    pub(super) fn operation(&self) -> Operation {
        let mut arguments = json!({"page": self.page});
        if let Some(tool) = &self.tool {
            arguments["tool"] = json!(tool);
        }
        Operation::dashboard_read("plugin", "page-action")
            .requires(&DASHBOARD_PLUGIN_PAGE_ACTION)
            .dashboard_row(
                &DASHBOARD_PLUGIN_PAGE_ACTION,
                self.namespace.clone(),
                arguments,
                DashboardOrigin {
                    host: self.host.clone(),
                    session: self.session.clone(),
                    ..Default::default()
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
    let (verb, resolved) = page_tool(&source, namespace, tool)?;
    if resolved.execution_kind != PluginExecutionKind::ReadOnly {
        return Err(OrbitError::PolicyDenied(format!(
            "tool '{verb}' of plugin '{namespace}' may change state; a page calls one only as \
             an action the operator confirms"
        )));
    }
    if !source.page().tools.iter().any(|declared| declared == verb) {
        return Err(OrbitError::PolicyDenied(format!(
            "page '{page_id}' of plugin '{namespace}' does not declare tool '{verb}'; list \
                 it under the page's `tools`"
        )));
    }
    check_input(namespace, verb, resolved, &input)?;
    Ok(PageCall {
        runtime: core.runtime().clone(),
        tool: source.plugin.tool_name(verb, source.first_party()),
        input,
    })
}

/// Check one action request from an active page: the tool is this plugin's
/// and mutating, the page names it under `actions`, the loaded action
/// matches the recorded consent, and `input` fits its schema.
/// Nothing runs.
pub(super) fn check_page_action(
    core: &Core,
    namespace: &str,
    page_id: &str,
    tool: &str,
    input: Value,
) -> Result<PageAction, OrbitError> {
    let source = resolve_page(core, namespace, page_id)?;
    let (verb, resolved) = page_tool(&source, namespace, tool)?;
    if resolved.execution_kind != PluginExecutionKind::Mutating {
        return Err(OrbitError::PolicyDenied(format!(
            "tool '{verb}' of plugin '{namespace}' is `read_only`; a page reads it with a call, \
             not an action"
        )));
    }
    if !source
        .page()
        .actions
        .iter()
        .any(|declared| declared == verb)
    {
        return Err(OrbitError::PolicyDenied(format!(
            "page '{page_id}' of plugin '{namespace}' does not declare action '{verb}'; list it \
             under the page's `actions`"
        )));
    }
    let runtime = core.runtime();
    let installed = runtime
        .stores()
        .host_store()
        .read()
        .and_then(|snapshot| snapshot.plugin().get(namespace))?;
    if !installed.as_ref().is_some_and(|installed| {
        grants::witnessed_page_actions(&runtime.global_root(), installed).allows_action(
            installed,
            &source.plugin,
            source.page(),
            resolved,
        )
    }) {
        return Err(OrbitError::PolicyDenied(format!(
            "action '{verb}' of page '{page_id}' of plugin '{namespace}' has no recorded \
             consent; review the page's actions with `orbit plugin enable {namespace}`"
        )));
    }
    check_input(namespace, verb, resolved, &input)?;
    Ok(PageAction {
        plugin: namespace.to_string(),
        version: source.plugin.manifest.metadata.version.clone(),
        page: page_id.to_string(),
        page_title: source.page().title.clone(),
        tool: source.plugin.tool_name(verb, source.first_party()),
        description: resolved.description.clone(),
        input,
    })
}

/// Run one confirmed action: refused unless its confirmation held, then
/// checked again as [`check_page_action`] checks it, then run once through
/// the audited dispatch with the operator's capability and the web session.
pub(super) fn run_page_action(core: &Core, run: PageActionRun) -> Result<Value, OrbitError> {
    let PageActionRun {
        namespace,
        page,
        tool,
        input,
        confirmation,
        session,
        ..
    } = run;
    confirmation.map_err(OrbitError::PolicyDenied)?;
    let Some(tool) = tool else {
        return Err(OrbitError::InvalidInput(
            "a page action names its `tool`".to_string(),
        ));
    };
    let action = check_page_action(core, &namespace, &page, &tool, input)?;
    core.runtime().run_tool_command_with_session(
        &action.tool,
        action.input,
        None,
        None,
        ToolSessionContext {
            effective_capabilities: [McpCapability::Operator].into_iter().collect(),
            origin_session_id: session,
            write_surface: Some(TaskWriteSurface::Web),
            ..ToolSessionContext::default()
        },
    )
}

/// The tool of `source`'s plugin a page names, by verb or canonical name. A
/// dotted name of another plugin's tool, or a verb the plugin lacks, is a
/// policy refusal.
fn page_tool<'a>(
    source: &'a PageSource,
    namespace: &str,
    tool: &'a str,
) -> Result<(&'a str, &'a ResolvedPluginTool), OrbitError> {
    let prefix = plugin_tool_name(namespace, "", source.first_party());
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
    let Some(resolved) = source
        .plugin
        .tools
        .iter()
        .find(|candidate| candidate.verb == verb)
    else {
        return Err(OrbitError::PolicyDenied(format!(
            "plugin '{namespace}' has no tool '{verb}'"
        )));
    };
    Ok((verb, resolved))
}

fn check_input(
    namespace: &str,
    verb: &str,
    resolved: &ResolvedPluginTool,
    input: &Value,
) -> Result<(), OrbitError> {
    let schema = CompiledSchema::compile(resolved.input_schema.clone()).map_err(|error| {
        OrbitError::Execution(format!(
            "input schema of tool '{verb}' of plugin '{namespace}': {error}"
        ))
    })?;
    if let Some(violations) = schema.violations(input) {
        return Err(OrbitError::InvalidInput(format!(
            "input for tool '{verb}' does not fit its schema: {violations}"
        )));
    }
    Ok(())
}
