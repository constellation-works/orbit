//! Machine-bound MCP host state, definitions and workspace calls.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use orbit_cmd::registry_runtime::{RegisteredRuntimeFactory, ResolvedWorkspaceSelection};
use orbit_cmd::task_owner::{self, WorkspaceIdentity};
use orbit_common::protocol::tool_input::required_string;
use orbit_common::{NotFoundKind, OrbitError};
use orbit_core::OrbitRuntime;
use orbit_core::adapter::command::{
    HostPluginMcpCache, HostPluginMcpSurface, ToolEntryPoint,
    execute_global_in_process_tool_dispatch, execute_global_plugin_tool,
};
use orbit_core::runtime::HostLifetime;
use orbit_mcp::federated;
use orbit_mcp::{McpHost, McpSessionAuthority};
use orbit_types::tool::{McpToolDefinition, ToolSessionContext};
use serde_json::Value;

use super::dispatch::{call_workspace_selector, execute_core_tool};
use super::runtime_cache::{
    OPENED_WORKSPACE_RUNTIME_LOG, RUNTIME_OPEN_LOG_TARGET, WorkspaceRuntimeCache,
};
use super::serve::ID_RESOLVED_WORKSPACE_TOOLS;

/// One MCP server bound to the executing machine.
pub(super) struct ServerMcpHost {
    pub(super) global_root: PathBuf,
    process_machine_id: String,
    process_machine_name: String,
    /// What every discovery envelope says about this host [ORB-14448].
    host_facts: orbit_mcp::HostFacts,
    /// Runtimes this long-lived host has already opened.
    workspace_runtimes: WorkspaceRuntimeCache,
    pub(super) plugin_surface: HostPluginMcpCache,
    definitions: Mutex<Option<CachedMcpDefinitions>>,
}

/// The combined tool surface and the plugin snapshot it was built from.
struct CachedMcpDefinitions {
    plugins: Arc<HostPluginMcpSurface>,
    value: Arc<Vec<McpToolDefinition>>,
}

impl ServerMcpHost {
    pub(super) fn new(
        global_root: PathBuf,
        process_machine_id: String,
        process_machine_name: String,
    ) -> Self {
        let host_facts = orbit_cmd::hosts::local_host_facts(&global_root, &process_machine_id);
        Self {
            global_root,
            process_machine_id,
            process_machine_name,
            host_facts,
            workspace_runtimes: WorkspaceRuntimeCache::default(),
            plugin_surface: HostPluginMcpCache::default(),
            definitions: Mutex::new(None),
        }
    }

    pub(super) fn definition(&self, name: &str) -> Result<McpToolDefinition, OrbitError> {
        self.advertised_definitions()?
            .iter()
            .find(|definition| definition.schema.name == name)
            .cloned()
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::Tool, name.to_string()))
    }

    /// Reuse the validated combined surface while the plugin snapshot is unchanged.
    /// The plugin cache retains config and its store handle; row and file stamps
    /// pick up lifecycle changes and admission-file edits on the next request.
    pub(super) fn advertised_definitions(&self) -> Result<Arc<Vec<McpToolDefinition>>, OrbitError> {
        let plugins = match self.plugin_surface.resolve(&self.global_root) {
            Ok(plugins) => Some(plugins),
            Err(error) => {
                tracing::warn!(target: "orbit.mcp.plugin", error = %error,
                    "omitting plugin tools from tools/list");
                None
            }
        };
        let mut cached = self
            .definitions
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(plugins) = &plugins
            && let Some(previous) = cached.as_ref()
            && Arc::ptr_eq(&previous.plugins, plugins)
        {
            return Ok(Arc::clone(&previous.value));
        }
        let mut definitions = orbit_mcp::canonical_mcp_tool_definitions()
            .map_err(|error| OrbitError::InvalidInput(error.to_string()))?;
        if let Some(plugins) = &plugins {
            definitions.extend(plugins.definitions.iter().cloned());
        }
        definitions.sort_by(|left, right| left.schema.name.cmp(&right.schema.name));
        orbit_types::tool::validate_mcp_tool_definitions(&definitions)
            .map_err(|error| OrbitError::InvalidInput(error.to_string()))?;
        let definitions = Arc::new(definitions);
        *cached = plugins.map(|plugins| CachedMcpDefinitions {
            plugins,
            value: Arc::clone(&definitions),
        });
        Ok(definitions)
    }

    /// Plugin classification uses the same snapshot as discovery.
    pub(super) fn is_plugin_tool(&self, name: &str) -> bool {
        self.plugin_surface
            .resolve(&self.global_root)
            .is_ok_and(|surface| {
                surface
                    .definitions
                    .iter()
                    .any(|definition| definition.schema.name == name)
            })
    }

    /// The plugins every registered checkout on this host has switched off.
    ///
    /// Reads each checkout's `[plugin_enablement]` table only, not a whole
    /// runtime: an unbound `tools/list` should not open every workspace. No
    /// registered checkout means no workspace to be off in, so nothing is.
    pub(super) fn plugins_off_in_every_workspace<'a>(
        &self,
        plugins: impl Iterator<Item = &'a String>,
    ) -> BTreeSet<String> {
        let registry_path =
            orbit_registry::workspace_registry::registry_path_for(&self.global_root);
        let Ok(registry) = orbit_registry::workspace_registry::load_registry_from(&registry_path)
        else {
            return BTreeSet::new();
        };
        if registry.checkouts.is_empty() {
            return BTreeSet::new();
        }
        let mut toggles = Vec::with_capacity(registry.checkouts.len());
        for checkout in &registry.checkouts {
            let roots = orbit_config::ConfigRoots::new(&self.global_root, &checkout.orbit_dir);
            match orbit_config::load_workspace_plugin_enablement(&roots) {
                Ok(checkout_toggles) => toggles.push(checkout_toggles),
                // An unreadable checkout may still serve the plugin.
                Err(_) => return BTreeSet::new(),
            }
        }
        plugins
            .filter(|plugin| {
                toggles
                    .iter()
                    .all(|checkout| checkout.get(plugin.as_str()) == Some(&false))
            })
            .cloned()
            .collect()
    }

    /// The selector this call lands in: the call's own, else the session's.
    fn workspace_selector<'a>(
        explicit: Option<&'a str>,
        context: &'a ToolSessionContext,
    ) -> Option<&'a str> {
        explicit.or_else(|| {
            context
                .workspace
                .as_deref()
                .map(str::trim)
                .filter(|selector| !selector.is_empty())
        })
    }

    fn workspace_required(&self, name: &str) -> OrbitError {
        OrbitError::InvalidInput(format!(
            "tool '{name}' requires an explicit workspace selector; first call \
             `orbit_workspace_list` and reuse a returned `ws_*` ID as `workspace`. \
             If no workspace is listed, run `orbit init` and then `orbit workspace init` \
             from the project directory. A selector may also be passed in MCP initialize \
             metadata; Orbit never infers one from the server process cwd"
        ))
    }

    fn list_workspaces(&self, input: &Value) -> Result<Value, OrbitError> {
        let include_crews = orbit_mcp::workspace_list_includes_crews(input)?;
        let registry_path =
            orbit_registry::workspace_registry::registry_path_for(&self.global_root);
        let registry = orbit_registry::workspace_registry::load_registry_from(&registry_path)?;
        let mut listing =
            orbit_mcp::execute_discovery_tool("orbit.workspace.list", &registry, &self.host_facts)?;
        if include_crews {
            self.attach_crews(&mut listing);
        }
        Ok(listing)
    }

    fn list_federated_workspaces(&self, input: &Value) -> Result<Value, OrbitError> {
        let include_crews = orbit_mcp::workspace_list_includes_crews(input)?;
        let registry_path =
            orbit_registry::workspace_registry::registry_path_for(&self.global_root);
        let registry = orbit_registry::workspace_registry::load_registry_from(&registry_path)?;
        let mut listing =
            orbit_mcp::execute_federated_workspace_discovery(&registry, &self.host_facts);
        if include_crews {
            self.attach_crews(&mut listing);
        }
        Ok(listing)
    }

    /// Add each listed workspace's effective crews, read from its own
    /// runtime. A workspace whose runtime cannot open reports `crews_error`
    /// instead, so one broken checkout does not hide the others' crews.
    fn attach_crews(&self, listing: &mut Value) {
        let Some(rows) = listing.get_mut("workspaces").and_then(Value::as_array_mut) else {
            return;
        };
        for row in rows {
            let Some(workspace_id) = row.get("id").and_then(Value::as_str).map(str::to_string)
            else {
                continue;
            };
            match self.workspace_crews(&workspace_id) {
                Ok(crews) => row["crews"] = crews,
                Err(error) => row["crews_error"] = Value::String(error.to_string()),
            }
        }
    }

    fn workspace_crews(&self, workspace_id: &str) -> Result<Value, OrbitError> {
        let selected =
            RegisteredRuntimeFactory::resolve_workspace_selector(&self.global_root, workspace_id)?;
        let runtime = self.open_selected_runtime(&selected)?;
        let discovery = runtime.crew_discovery(
            &selected.workspace.id,
            selected.workspace.owner_machine_id.clone(),
        )?;
        serde_json::to_value(discovery)
            .map_err(|error| OrbitError::Execution(format!("serialize crew discovery: {error}")))
    }

    pub(super) fn call_global_tool(
        &self,
        name: &str,
        input: Value,
        context: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        execute_global_in_process_tool_dispatch(
            &self.global_root,
            name,
            input,
            ToolEntryPoint::Mcp,
            context,
            |input| match name {
                "orbit.workspace.list" => self.list_workspaces(&input),
                orbit_mcp::FEDERATED_DESTINATION_WORKSPACE_LIST_TOOL => {
                    self.list_federated_workspaces(&input)
                }
                _ => Err(OrbitError::not_found(NotFoundKind::Tool, name.to_string())),
            },
        )
        .map(|outcome| outcome.value)
    }

    /// A `mcp_scope: global` plugin tool: no workspace to open, so it runs
    /// against the host's plugin records inside Core's audited dispatch.
    pub(super) fn call_global_plugin_tool(
        &self,
        name: &str,
        input: Value,
        context: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        execute_global_plugin_tool(&self.global_root, name, input, ToolEntryPoint::Mcp, context)
    }

    pub(super) fn resolve_workspace_runtime(
        &self,
        name: &str,
        input: &Value,
        context: &ToolSessionContext,
    ) -> Result<(Arc<OrbitRuntime>, ResolvedWorkspaceSelection), OrbitError> {
        let selected = self.workspace_selection(name, input, context)?;
        let runtime = self.open_selected_runtime(&selected)?;
        Ok((runtime, selected))
    }

    /// The shared long-lived runtime for one resolved checkout.
    fn open_selected_runtime(
        &self,
        selected: &ResolvedWorkspaceSelection,
    ) -> Result<Arc<OrbitRuntime>, OrbitError> {
        self.workspace_runtimes
            .resolve(&self.global_root, selected, || {
                let runtime = RegisteredRuntimeFactory::open_registered_checkout_for(
                    &self.global_root,
                    &selected.workspace,
                    &selected.checkout,
                    HostLifetime::LongLived,
                )?;
                tracing::debug!(
                    target: RUNTIME_OPEN_LOG_TARGET,
                    workspace_id = %selected.workspace.id,
                    "{OPENED_WORKSPACE_RUNTIME_LOG}"
                );
                Ok(runtime)
            })
    }

    /// Which registered workspace this call lands in.
    ///
    /// `orbit.task.show` and `orbit.task.artifact.get` follow the globally
    /// unique task ID unless the call itself passes `workspace` [ORB-10797]
    /// [ORB-10961] [ORB-12254]: the session's announced workspace is ambient,
    /// like cwd, and is the right default for authoring but the wrong one for
    /// addressing an ID. Linked-worktree runtime identities are also ambient
    /// and must not become a filter. An explicit per-call `workspace` stays a
    /// filter on every tool, so a task owned elsewhere is not found there.
    /// The other id-routed task tools resolve their id the same way when the
    /// session is unbound, which is how a federated route that carries only
    /// the id lands [ORB-14449].
    fn workspace_selection(
        &self,
        name: &str,
        input: &Value,
        context: &ToolSessionContext,
    ) -> Result<ResolvedWorkspaceSelection, OrbitError> {
        let explicit = call_workspace_selector(input)?;
        if explicit.is_none() && federated::is_id_routed_tool(name) {
            let task_id = required_string(input, &["id"], "id")?;
            // A v1 server does not relay [ORB-14449]: an id another
            // registered host writes is refused with where it lives, never
            // answered from a local mirror or reported as not found.
            if let Some(holder) = orbit_cmd::hosts::remote_task_holder(&self.global_root, &task_id)?
            {
                return Err(orbit_cmd::hosts::task_prefix_remote(&task_id, &holder));
            }
            // An unbound session — including a federated route that carried
            // only the id — resolves it through this host's task registry.
            if ID_RESOLVED_WORKSPACE_TOOLS.contains(&name)
                || Self::workspace_selector(None, context).is_none()
            {
                return task_owner::resolve_task_owner(&self.global_root, &task_id);
            }
        }
        let selector = Self::workspace_selector(explicit, context)
            .ok_or_else(|| self.workspace_required(name))?;
        RegisteredRuntimeFactory::resolve_workspace_selector(&self.global_root, selector)
    }

    pub(super) fn audit_global_failure(
        &self,
        name: &str,
        input: Value,
        context: ToolSessionContext,
        error: OrbitError,
    ) -> Result<Value, OrbitError> {
        execute_global_in_process_tool_dispatch(
            &self.global_root,
            name,
            input,
            ToolEntryPoint::Mcp,
            context,
            move |_| Err(error),
        )
        .map(|outcome| outcome.value)
    }

    pub(super) fn call_workspace_tool(
        &self,
        name: &str,
        mut input: Value,
        mut context: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        if let Some(binding) = &context.worker_invocation
            && binding.execution.machine_id == self.process_machine_id
            && binding.owner_machine_id != self.process_machine_id
            && (name.starts_with("orbit.task.") || name.starts_with("orbit.friction."))
        {
            let routed_name = if name == "orbit.task.artifact.put" {
                let cwd = std::env::current_dir()?;
                input = orbit_cmd::prepare_remote_task_artifact_put(
                    input,
                    Some(&cwd),
                    Some(&cwd),
                    context.transport,
                )?;
                name
            } else {
                name
            };
            if let Some(selector) = call_workspace_selector(&input)?
                && selector != binding.owner_destination
                && selector != binding.owner_workspace_id
                && !std::env::current_dir().ok().is_some_and(|cwd| {
                    std::fs::canonicalize(selector).is_ok_and(|path| path == cwd)
                })
            {
                return Err(OrbitError::PolicyDenied(
                    "worker workspace binding mismatch".into(),
                ));
            }
            if let Some(object) = input.as_object_mut() {
                object.insert(
                    "workspace".into(),
                    Value::String(binding.owner_destination.clone()),
                );
            }
            let remotes = federated::load_destinations(&self.global_root)?;
            let destinations = federated::federated_membership(
                self.process_machine_id.clone(),
                self.process_machine_name.clone(),
                remotes,
            );
            context.workspace = Some(binding.owner_destination.clone());
            let probe = federated::SshDestinationProbe::new(
                self.process_machine_id.clone(),
                federated::DEFAULT_PROBE_TIMEOUT,
                federated::DEFAULT_ROUTED_DELIVERY_TIMEOUT,
                context.orchestrator.clone(),
                McpSessionAuthority::Agent,
            );
            return federated::FederatedMcpHost::new(destinations, Arc::new(probe)).call_tool(
                routed_name,
                input,
                context,
            );
        }
        let (runtime, selected) = match self.resolve_workspace_runtime(name, &input, &context) {
            Ok(resolved) => resolved,
            Err(error) => {
                return self.audit_global_failure(name, input, context, error);
            }
        };
        let repo_root = selected.checkout.repo_root.to_string_lossy().into_owned();

        context.workspace_id = Some(selected.workspace.id.clone());
        context.workspace = Some(repo_root.clone());
        context.process_machine_id = Some(self.process_machine_id.clone());
        context.process_machine_name = Some(self.process_machine_name.clone());

        if let Some(object) = input.as_object_mut()
            && object.contains_key("workspace")
        {
            object.insert("workspace".to_string(), Value::String(repo_root));
        }

        // Destination catalog-role gate [ORB-11021]: refuse before the tool body
        // runs. Unclassified and execute-class tools pass and keep their own auth.
        if let Err(error) = federated::ensure_tool_class_held(
            name,
            federated::CapabilityClasses::for_checkout(&selected.workspace, &selected.checkout),
        ) {
            return runtime
                .execute_in_process_tool_dispatch(
                    name,
                    input,
                    ToolEntryPoint::Mcp,
                    context,
                    move |_| Err(error),
                )
                .map(|outcome| outcome.value);
        }

        let owner = WorkspaceIdentity {
            id: selected.workspace.id.clone(),
            name: selected.workspace.name.clone(),
        };
        execute_core_tool(&runtime, name, input, context, Some(&owner))
    }
}
