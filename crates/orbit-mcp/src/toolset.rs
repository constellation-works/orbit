//! `orbit mcp serve` toolsets over any [`McpHost`].
//!
//! A server advertises the definitions of its enabled toolsets and refuses a
//! call to any other definition with a structured `toolset_not_enabled`
//! refusal naming the toolset and how to enable it, never "unknown tool". The
//! tools stay registered: only this server's listing and dispatch change, so
//! `orbit tool run`, activities and plugin callback sessions are unaffected.
//! Each definition's toolset comes from its registry rules; nothing here names
//! a tool.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, PoisonError};

use orbit_common::{OrbitError, RemoteToolCode};
use orbit_types::tool::{
    McpToolDefinition, McpToolset, McpToolsetSelection, ToolSessionContext,
    mcp_advertised_tool_name,
};
use serde_json::{Value, json};

use crate::McpHost;

/// The host config key that selects a server's toolsets.
pub const MCP_TOOLSETS_CONFIG_KEY: &str = "machine.mcp.toolsets";

/// The refusal code of a call to a tool outside the server's toolsets.
pub const TOOLSET_NOT_ENABLED_CODE: &str = "toolset_not_enabled";

/// Serve `inner` with only the definitions of `selection`'s toolsets.
pub fn with_mcp_toolsets(
    inner: Arc<dyn McpHost>,
    selection: McpToolsetSelection,
) -> Arc<dyn McpHost> {
    if selection == McpToolsetSelection::all() {
        return inner;
    }
    Arc::new(ToolsetFilteredHost {
        inner,
        selection,
        surface: Mutex::new(None),
    })
}

struct ToolsetFilteredHost {
    inner: Arc<dyn McpHost>,
    selection: McpToolsetSelection,
    surface: Mutex<Option<Arc<FilteredSurface>>>,
}

/// One inner surface split into what this server advertises and what it
/// withholds, keyed by both canonical and advertised names.
struct FilteredSurface {
    source: Arc<Vec<McpToolDefinition>>,
    advertised: Arc<Vec<McpToolDefinition>>,
    withheld: HashMap<String, (String, McpToolset)>,
}

impl ToolsetFilteredHost {
    fn surface(&self) -> Result<Arc<FilteredSurface>, OrbitError> {
        let dynamic = self.inner.dynamic_mcp_tool_definitions()?;
        let mut cached = self.surface.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(surface) = cached.as_ref()
            && dynamic
                .as_ref()
                .is_none_or(|source| Arc::ptr_eq(source, &surface.source))
        {
            return Ok(Arc::clone(surface));
        }
        let source = match dynamic {
            Some(source) => source,
            None => Arc::new(self.inner.list_mcp_tool_definitions()?),
        };
        let mut advertised = Vec::with_capacity(source.len());
        let mut withheld = HashMap::new();
        for definition in source.iter() {
            let toolset = definition.rules.toolset;
            if self.selection.enables(toolset) {
                advertised.push(definition.clone());
            } else {
                let canonical = definition.schema.name.clone();
                withheld.insert(
                    mcp_advertised_tool_name(&canonical),
                    (canonical.clone(), toolset),
                );
                withheld.insert(canonical.clone(), (canonical, toolset));
            }
        }
        let surface = Arc::new(FilteredSurface {
            source,
            advertised: Arc::new(advertised),
            withheld,
        });
        *cached = Some(Arc::clone(&surface));
        Ok(surface)
    }
}

/// The refusal a long-lived client gets for a tool this server withholds.
fn toolset_not_enabled(tool: &str, toolset: McpToolset) -> OrbitError {
    let flag = format!("--toolset {toolset}");
    let message = format!(
        "tool '{tool}' is in the '{toolset}' MCP toolset, which this server does not \
         enable; restart `orbit mcp serve` with `{flag}` or add '{toolset}' to \
         `{MCP_TOOLSETS_CONFIG_KEY}`. `orbit tool run {tool}` reaches it without MCP"
    );
    OrbitError::RemoteTool {
        code: RemoteToolCode::Other(TOOLSET_NOT_ENABLED_CODE.into()),
        payload: json!({
            "code": TOOLSET_NOT_ENABLED_CODE,
            "message": message,
            "tool": tool,
            "toolset": toolset,
            "enable": {
                "flag": flag,
                "config_key": MCP_TOOLSETS_CONFIG_KEY,
            },
        }),
        message,
    }
}

impl McpHost for ToolsetFilteredHost {
    fn list_mcp_tool_definitions(&self) -> Result<Vec<McpToolDefinition>, OrbitError> {
        self.surface()
            .map(|surface| surface.advertised.as_ref().clone())
    }

    fn dynamic_mcp_tool_definitions(
        &self,
    ) -> Result<Option<Arc<Vec<McpToolDefinition>>>, OrbitError> {
        if self.inner.dynamic_mcp_tool_definitions()?.is_none() {
            return Ok(None);
        }
        self.surface()
            .map(|surface| Some(Arc::clone(&surface.advertised)))
    }

    fn friction_tag_taxonomy(
        &self,
        session_context: &ToolSessionContext,
    ) -> Result<Option<Vec<(String, String)>>, OrbitError> {
        self.inner.friction_tag_taxonomy(session_context)
    }

    fn hidden_tool_names(&self, session_context: &ToolSessionContext) -> BTreeSet<String> {
        self.inner.hidden_tool_names(session_context)
    }

    fn call_tool(
        &self,
        name: &str,
        input: Value,
        session_context: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        if let Some((canonical, toolset)) = self.surface()?.withheld.get(name) {
            return Err(toolset_not_enabled(canonical, *toolset));
        }
        self.inner.call_tool(name, input, session_context)
    }

    fn call_internal_drain(
        &self,
        name: &str,
        input: Value,
        context: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        self.inner.call_internal_drain(name, input, context)
    }

    fn refuse_internal_drain(
        &self,
        name: &str,
        input: Value,
        context: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        self.inner.refuse_internal_drain(name, input, context)
    }

    fn federated_workspace_selectors(&self) -> bool {
        self.inner.federated_workspace_selectors()
    }
}
