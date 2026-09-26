use std::collections::HashMap;
use std::sync::Arc;

use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::tool::{
    McpToolDefinition, McpToolDefinitionError, McpToolScope, ToolSchema, mcp_advertised_tool_name,
    validate_mcp_tool_definitions,
};
use serde_json::Value;

use crate::plugin::PluginToolBinding;
use crate::{Tool, ToolContext, ToolExecutionKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolAvailability {
    Active,
    Inactive,
}

impl ToolAvailability {
    pub fn is_active(self) -> bool {
        matches!(self, Self::Active)
    }
}

struct ToolEntry {
    tool: Arc<dyn Tool>,
    availability: ToolAvailability,
    mcp_scope: Option<McpToolScope>,
    /// Set for a plugin-backed entry: provenance for the audit row and, on
    /// an inactive entry, the diagnostic naming the missing step.
    plugin: Option<Arc<PluginToolBinding>>,
}

#[derive(Default)]
pub struct ToolRegistry {
    tools: HashMap<String, ToolEntry>,
    mcp_registration_error: Option<McpToolDefinitionError>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
            mcp_registration_error: None,
        }
    }

    pub fn register<T: Tool + 'static>(&mut self, tool: T) {
        self.register_with_availability(tool, ToolAvailability::Active, None);
    }

    /// Register one active builtin for MCP advertisement at the given scope.
    pub fn register_mcp<T: Tool + 'static>(&mut self, tool: T, scope: McpToolScope) {
        self.register_with_availability(tool, ToolAvailability::Active, Some(scope));
    }

    pub fn register_inactive<T: Tool + 'static>(&mut self, tool: T) {
        self.register_with_availability(tool, ToolAvailability::Inactive, None);
    }

    /// Register one plugin tool. `mcp_scope: None` keeps it off `tools/list`
    /// (`mcp_scope: none` in the manifest) while `orbit tool run` reaches it.
    pub fn register_plugin_tool<T: Tool + 'static>(
        &mut self,
        tool: T,
        mcp_scope: Option<McpToolScope>,
        binding: Arc<PluginToolBinding>,
    ) {
        self.register_entry(tool, ToolAvailability::Active, mcp_scope, Some(binding));
    }

    /// Register a plugin tool the host could not activate. The binding's
    /// diagnostic is what `orbit tool run` and `orbit plugin show` report.
    pub fn register_inactive_plugin_tool<T: Tool + 'static>(
        &mut self,
        tool: T,
        binding: Arc<PluginToolBinding>,
    ) {
        self.register_entry(tool, ToolAvailability::Inactive, None, Some(binding));
    }

    fn register_with_availability<T: Tool + 'static>(
        &mut self,
        tool: T,
        availability: ToolAvailability,
        mcp_scope: Option<McpToolScope>,
    ) {
        self.register_entry(tool, availability, mcp_scope, None);
    }

    fn register_entry<T: Tool + 'static>(
        &mut self,
        tool: T,
        availability: ToolAvailability,
        mcp_scope: Option<McpToolScope>,
        plugin: Option<Arc<PluginToolBinding>>,
    ) {
        let schema = tool.schema();
        let collision = self.tools.get(&schema.name).map(|existing| {
            (
                existing.mcp_scope.is_some() || mcp_scope.is_some(),
                existing.plugin.is_none() && existing.tool.schema().builtin,
                existing.availability.is_active(),
                existing
                    .plugin
                    .as_ref()
                    .map(|binding| binding.provenance.name.clone()),
            )
        });
        if let Some((mcp_collision, hold_builtin, existing_active, existing_owner)) = collision {
            if mcp_collision {
                self.record_mcp_error(McpToolDefinitionError::DuplicateCanonicalName(
                    schema.name.clone(),
                ));
            }
            // Built-ins register first, active or held inactive behind a
            // subcommand gate. A later insert must not replace one: a plugin
            // that collides would otherwise take the name.
            if hold_builtin {
                return;
            }
            // An active entry already answers for this name — another
            // plugin's, or a host-registered external tool's. A later
            // registration under the same name (e.g. a tampered manifest
            // re-claiming it) is refused rather than swapped in: §4.9 fails
            // closed per plugin, so one plugin's collision must not touch
            // another plugin's or an external tool's active entry.
            if existing_active {
                let new_owner = plugin
                    .as_ref()
                    .map(|binding| binding.provenance.name.clone());
                if existing_owner != new_owner {
                    return;
                }
            }
        }
        if mcp_scope.is_some() {
            let advertised_name = mcp_advertised_tool_name(&schema.name);
            if self.tools.iter().any(|(name, entry)| {
                entry.mcp_scope.is_some()
                    && name != &schema.name
                    && mcp_advertised_tool_name(name) == advertised_name
            }) {
                self.record_mcp_error(McpToolDefinitionError::DuplicateAdvertisedName(
                    advertised_name,
                ));
            }
        }
        self.tools.insert(
            schema.name,
            ToolEntry {
                tool: Arc::new(tool),
                availability,
                mcp_scope,
                plugin,
            },
        );
    }

    fn record_mcp_error(&mut self, error: McpToolDefinitionError) {
        if self.mcp_registration_error.is_none() {
            self.mcp_registration_error = Some(error);
        }
    }

    pub fn register_builtins(&mut self) {
        crate::builtin::register_builtins(self);
    }

    pub fn execute(
        &self,
        name: &str,
        ctx: &ToolContext,
        input: Value,
    ) -> Result<Value, OrbitError> {
        let tool = self
            .tools
            .get(name)
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::Tool, name.to_string()))?;
        tool.tool.execute(ctx, input)
    }

    pub fn get_schema(&self, name: &str) -> Option<ToolSchema> {
        self.tools.get(name).map(|entry| entry.tool.schema())
    }

    pub fn get_active_schema(&self, name: &str) -> Option<ToolSchema> {
        self.tools
            .get(name)
            .filter(|entry| entry.availability.is_active())
            .map(|entry| entry.tool.schema())
    }

    pub fn has(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    pub fn availability(&self, name: &str) -> Option<ToolAvailability> {
        self.tools.get(name).map(|entry| entry.availability)
    }

    pub fn is_active(&self, name: &str) -> bool {
        self.availability(name)
            .is_some_and(ToolAvailability::is_active)
    }

    pub fn execution_kind(&self, name: &str) -> Option<ToolExecutionKind> {
        self.tools
            .get(name)
            .map(|entry| entry.tool.execution_kind())
    }

    /// The plugin behind a registry entry, when it is plugin-backed.
    pub fn plugin_binding(&self, name: &str) -> Option<Arc<PluginToolBinding>> {
        self.tools
            .get(name)
            .and_then(|entry| entry.plugin.as_ref().map(Arc::clone))
    }

    /// Why a plugin tool is inactive, when the loader recorded a reason.
    pub fn inactive_diagnostic(&self, name: &str) -> Option<String> {
        self.plugin_binding(name)
            .and_then(|binding| binding.diagnostic.clone())
    }

    /// Advertised MCP scope of one entry, active or not.
    pub fn mcp_scope(&self, name: &str) -> Option<McpToolScope> {
        self.tools.get(name).and_then(|entry| entry.mcp_scope)
    }

    pub fn unregister(&mut self, name: &str) -> bool {
        self.tools.remove(name).is_some()
    }

    pub fn schemas(&self) -> Vec<ToolSchema> {
        self.tools
            .values()
            .filter(|entry| entry.availability.is_active())
            .map(|entry| entry.tool.schema())
            .collect()
    }

    pub fn all_schemas(&self) -> Vec<ToolSchema> {
        self.tools
            .values()
            .map(|entry| entry.tool.schema())
            .collect()
    }

    /// Enumerate validated, active builtin MCP definitions without runtime or workspace state.
    pub fn mcp_tool_definitions(&self) -> Result<Vec<McpToolDefinition>, McpToolDefinitionError> {
        if let Some(error) = &self.mcp_registration_error {
            return Err(error.clone());
        }
        let mut definitions = self
            .tools
            .values()
            .filter(|entry| entry.availability.is_active())
            .filter_map(|entry| {
                entry.mcp_scope.map(|scope| {
                    McpToolDefinition::new(entry.tool.schema(), scope)
                        .with_input_schema(entry.tool.input_schema())
                })
            })
            .collect::<Vec<_>>();
        definitions.sort_by(|left, right| left.schema.name.cmp(&right.schema.name));
        validate_mcp_tool_definitions(&definitions)?;
        Ok(definitions)
    }
}

/// Workspace-independent source for every canonical registry-backed MCP definition.
pub fn canonical_builtin_mcp_tool_definitions()
-> Result<Vec<McpToolDefinition>, McpToolDefinitionError> {
    let mut registry = ToolRegistry::new();
    registry.register_builtins();
    registry.mcp_tool_definitions()
}
