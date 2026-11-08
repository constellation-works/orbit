use std::collections::HashMap;
use std::sync::Arc;

use orbit_common::OrbitError;
use orbit_types::tool::{
    McpIdRoute, McpToolDefinition, McpToolDefinitionError, McpToolScope, McpToolset, ToolSchema,
    mcp_advertised_tool_name, validate_mcp_tool_definitions,
};
use serde_json::Value;

use crate::mcp::{builtin_mcp_toolset, mcp_annotations, mcp_rules};
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
    /// The `orbit mcp serve` toolset this entry belongs to: a builtin MCP
    /// tool's comes from [`crate::mcp::builtin_mcp_toolset`], a plugin's is
    /// [`McpToolset::Plugins`] and a tool MCP never advertises is
    /// [`McpToolset::Local`]. `None` only for a builtin MCP tool missing from
    /// the toolset table, which [`ToolRegistry::mcp_tool_definitions`]
    /// refuses.
    toolset: Option<McpToolset>,
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

    /// Register one active builtin for MCP advertisement at the given scope,
    /// in the toolset the builtin toolset table places it.
    pub fn register_mcp<T: Tool + 'static>(&mut self, tool: T, scope: McpToolScope) {
        let toolset = builtin_mcp_toolset(&tool.schema().name);
        self.register_entry(tool, ToolAvailability::Active, Some(scope), toolset, None);
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
        self.register_entry(
            tool,
            ToolAvailability::Active,
            mcp_scope,
            Some(McpToolset::Plugins),
            Some(binding),
        );
    }

    /// Register a plugin tool the host could not activate. The binding's
    /// diagnostic is what `orbit tool run` and `orbit plugin show` report.
    pub fn register_inactive_plugin_tool<T: Tool + 'static>(
        &mut self,
        tool: T,
        binding: Arc<PluginToolBinding>,
    ) {
        self.register_entry(
            tool,
            ToolAvailability::Inactive,
            None,
            Some(McpToolset::Plugins),
            Some(binding),
        );
    }

    fn register_with_availability<T: Tool + 'static>(
        &mut self,
        tool: T,
        availability: ToolAvailability,
        mcp_scope: Option<McpToolScope>,
    ) {
        self.register_entry(tool, availability, mcp_scope, Some(McpToolset::Local), None);
    }

    fn register_entry<T: Tool + 'static>(
        &mut self,
        tool: T,
        availability: ToolAvailability,
        mcp_scope: Option<McpToolScope>,
        toolset: Option<McpToolset>,
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
                toolset,
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
        let tool = self.tools.get(name).ok_or_else(|| {
            crate::tool_suggestions::unknown_tool_error(
                name,
                self.tools
                    .iter()
                    .filter(|(_, entry)| {
                        entry.availability.is_active()
                            && entry.mcp_scope.is_some()
                            && entry.plugin.is_none()
                            && entry.tool.schema().builtin
                    })
                    .map(|(name, _)| name.as_str()),
                ctx,
            )
        })?;
        if name.starts_with("orbit.task.")
            // Pull owns typed protocol negotiation in the host adapter, which
            // checks the fingerprint before validating the derived request
            // shape. This registry cannot know the owner's request types.
            && name != "orbit.task.pull"
            && tool.plugin.is_none()
            && !is_preloaded_artifact_payload(name, &input)
        {
            let aliases = declared_aliases(name);
            let retired_fields = declared_retired_fields(name);
            validate_task_arguments(
                &input,
                &tool.tool.schema(),
                cli_fields(name, ctx),
                &aliases,
                &retired_fields,
            )?;
        }
        tool.tool.execute(ctx, input)
    }

    pub fn get_schema(&self, name: &str) -> Option<ToolSchema> {
        self.tools.get(name).map(|entry| entry.tool.schema())
    }

    /// The tool's full declared input schema, including conditional requirements.
    /// Inspection includes inactive tools, just as [`Self::get_schema`] does.
    pub fn get_input_schema(&self, name: &str) -> Option<Value> {
        self.tools
            .get(name)
            .and_then(|entry| entry.tool.input_schema())
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

    /// The `orbit mcp serve` toolset of one entry, active or not:
    /// [`McpToolset::Local`] for an entry MCP never advertises, and `None`
    /// only for an unknown name or an unclassified builtin MCP tool.
    pub fn mcp_toolset(&self, name: &str) -> Option<McpToolset> {
        self.tools.get(name).and_then(|entry| entry.toolset)
    }

    /// Advertised MCP scope of one entry, active or not.
    pub fn mcp_scope(&self, name: &str) -> Option<McpToolScope> {
        self.tools.get(name).and_then(|entry| entry.mcp_scope)
    }

    /// Every registered tool's declared id route, resolved, by tool name.
    /// Includes tools MCP never advertises (CLI-only operations), whose
    /// name-based routing the CLI still consumes.
    pub(crate) fn declared_id_routes(&self) -> HashMap<String, McpIdRoute> {
        self.tools
            .values()
            .map(|entry| {
                let schema = entry.tool.schema();
                (schema.name, entry.tool.declared_id_route().resolve())
            })
            .collect()
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
                    let schema = entry.tool.schema();
                    let annotations = mcp_annotations(&schema, entry.tool.execution_kind());
                    let mut rules = mcp_rules(&schema, entry.tool.declared_id_route());
                    rules.toolset = entry.toolset.unwrap_or_default();
                    McpToolDefinition::new(schema, scope)
                        .with_rules(rules)
                        .with_input_schema(entry.tool.input_schema())
                        .with_annotations(Some(annotations))
                })
            })
            .collect::<Vec<_>>();
        definitions.sort_by(|left, right| left.schema.name.cmp(&right.schema.name));
        validate_mcp_tool_definitions(&definitions)?;
        // Fail closed in this target, where a tool-adding change is tested:
        // a builtin MCP tool that addresses one id with an optional
        // workspace must declare its id route at its definition site, and a
        // declared route must match that parameter shape.
        for definition in &definitions {
            if !definition.schema.builtin {
                continue;
            }
            let addresses = addresses_one_id_with_optional_workspace(definition);
            let name = definition.schema.name.clone();
            match (addresses, &definition.rules.id_route) {
                (true, McpIdRoute::Unspecified) => {
                    return Err(McpToolDefinitionError::UnclassifiedIdRoute(name));
                }
                (false, McpIdRoute::Task | McpIdRoute::WorkspaceLocal(_)) => {
                    return Err(McpToolDefinitionError::MisdeclaredIdRoute(name));
                }
                _ => {}
            }
        }
        let mut unclassified = self
            .tools
            .iter()
            .filter(|(_, entry)| entry.mcp_scope.is_some() && entry.toolset.is_none())
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        unclassified.sort();
        if let Some(name) = unclassified.into_iter().next() {
            return Err(McpToolDefinitionError::UnclassifiedToolset(name));
        }
        Ok(definitions)
    }
}

/// A spoke broker sends `orbit.task.artifact.put` a path-free `artifacts`
/// payload that is deliberately absent from the public schema. The handler
/// admits that shape only from the authenticated ssh-mcp connector and checks
/// its fields itself, so the public-schema check must not refuse it first.
fn is_preloaded_artifact_payload(name: &str, input: &Value) -> bool {
    name == "orbit.task.artifact.put" && input.get("artifacts").is_some()
}

/// Fields beyond the public schema a call may carry: the CLI options of a
/// task write the operator command built. Only an MCP session carries an
/// origin session, so an MCP caller is refused them as unknown fields.
fn cli_fields(name: &str, ctx: &ToolContext) -> &'static [&'static str] {
    const CLI_OPTIONS: &[&str] = &[orbit_common::governance::task::TASK_CLI_OPTIONS_PARAM];
    let task_write = name == "orbit.task.add" || name == "orbit.task.update";
    if task_write && ctx.session_context.origin_session_id.is_none() {
        CLI_OPTIONS
    } else {
        &[]
    }
}

/// The input spellings a task operation accepts besides its canonical names, as
/// `(canonical, alias)`. The schema lists canonical names only; the handler's
/// `OperationSpec::parse_input` renames an alias, so this check must let it
/// through rather than refuse it as an unknown field.
fn declared_aliases(name: &str) -> Vec<(&'static str, &'static str)> {
    orbit_common::governance::task::TASK_OPERATIONS
        .iter()
        .filter(|spec| spec.tool_name == name)
        .flat_map(|spec| spec.wire_params())
        .flat_map(|param| param.aliases.iter().map(|alias| (param.name, *alias)))
        .collect()
}

/// Retired names that must reach `OperationSpec::parse_input`.
///
/// `orbit.task.add` retires fields onto the operation that still accepts
/// them, and every task operation retires `agent` and the `context` alias.
/// Those refusals name the replacement. Other retired names stay refused
/// here, so their existing registry wording is unchanged.
fn declared_retired_fields(name: &str) -> Vec<&'static str> {
    orbit_common::governance::task::TASK_OPERATIONS
        .iter()
        .filter(|spec| spec.tool_name == name)
        .flat_map(|spec| spec.retired_fields.iter().map(|field| field.name))
        .filter(|field| name == "orbit.task.add" || *field == "agent" || *field == "context")
        .collect()
}

/// Task argument shapes are checked before a handler can ignore a mistyped
/// optional field or reach the application host. Required fields and richer
/// guarded modes remain the handler's responsibility.
fn validate_task_arguments(
    input: &Value,
    schema: &ToolSchema,
    cli_fields: &[&str],
    aliases: &[(&str, &str)],
    retired_fields: &[&str],
) -> Result<(), OrbitError> {
    use orbit_common::protocol::tool_input::{
        TOOL_INPUT_IDENTITY_KEYS, reject_unknown_tool_fields,
    };
    use orbit_common::protocol::tool_schema::tool_parameter_schema;

    let object = input
        .as_object()
        .ok_or_else(|| OrbitError::InvalidInput("task tool arguments must be an object".into()))?;
    // `model` stays an unadvertised identity key (CLI, `orbit tool run`, and
    // a caller that still sends it). The MCP schema does not list it.
    let allowed = schema
        .parameters
        .iter()
        .map(|param| param.name.as_str())
        .chain(cli_fields.iter().copied())
        .chain(aliases.iter().map(|(_, alias)| *alias))
        .chain(retired_fields.iter().copied())
        .chain(TOOL_INPUT_IDENTITY_KEYS.iter().copied())
        .collect::<Vec<_>>();
    reject_unknown_tool_fields(input, &allowed)?;
    for parameter in &schema.parameters {
        // A call that sets both spellings is refused by the handler; check
        // the canonical one here.
        let Some(value) = object.get(&parameter.name).or_else(|| {
            aliases
                .iter()
                .filter(|(canonical, _)| *canonical == parameter.name)
                .find_map(|(_, alias)| object.get(*alias))
        }) else {
            continue;
        };
        // Existing optional parsers treat null as absence, and accept numeric
        // strings and string booleans. Check their normalized shape without
        // changing the value passed to the handler.
        if value.is_null() && !parameter.required {
            continue;
        }
        let mut normalized = value.clone();
        // The persisted relation parser also accepts one relation object.
        if parameter.name == "relations" && value.is_object() {
            normalized = Value::Array(vec![value.clone()]);
        }
        if let Some(raw) = value.as_str() {
            match parameter.param_type.as_str() {
                "boolean" | "bool" => match raw.trim().to_ascii_lowercase().as_str() {
                    "true" => normalized = Value::Bool(true),
                    "false" => normalized = Value::Bool(false),
                    _ => {}
                },
                "integer" | "u64" => {
                    if let Ok(number) = raw.trim().parse::<u64>() {
                        normalized = Value::from(number);
                    }
                }
                // MCP advertises string lists as arrays. A single string is
                // still a list for the handler; check it as one element and
                // leave the value the handler sees unchanged.
                "string_list" | "string[]" | "strings" => {
                    normalized = Value::Array(vec![Value::String(raw.to_string())]);
                }
                _ => {}
            }
        }
        let shape = Value::Object(tool_parameter_schema(&parameter.param_type));
        let validator = jsonschema::JSONSchema::compile(&shape).map_err(|error| {
            OrbitError::Execution(format!("invalid task parameter schema: {error}"))
        })?;
        if !validator.is_valid(&normalized) {
            return Err(OrbitError::InvalidInput(format!(
                "`{}` must have type {}",
                parameter.name, parameter.param_type,
            )));
        }
    }
    Ok(())
}

/// Whether a definition's tool addresses exactly one id with an optional
/// workspace: a required `id` parameter, and a workspace that is either an
/// optional declared parameter or left to the selector the MCP host adds
/// to a non-global tool.
fn addresses_one_id_with_optional_workspace(definition: &McpToolDefinition) -> bool {
    let parameters = &definition.schema.parameters;
    let id_required = parameters
        .iter()
        .any(|parameter| parameter.name == "id" && parameter.required);
    let declared_workspace = parameters
        .iter()
        .find(|parameter| parameter.name == "workspace");
    let workspace_optional = match declared_workspace {
        Some(parameter) => !parameter.required,
        None => definition.scope != McpToolScope::Global,
    };
    id_required && workspace_optional
}

/// Workspace-independent source for every canonical registry-backed MCP definition.
pub fn canonical_builtin_mcp_tool_definitions()
-> Result<Vec<McpToolDefinition>, McpToolDefinitionError> {
    let mut registry = ToolRegistry::new();
    registry.register_builtins();
    registry.mcp_tool_definitions()
}
