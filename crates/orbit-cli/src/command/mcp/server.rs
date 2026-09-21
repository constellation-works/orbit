//! Local composition for the authoritative MCP server process.
//!
//! Both transports — stdio and the TCP listener — serve the same host with the
//! same trusted session envelope, so a call's dispatch and audit path does not
//! depend on how its bytes arrived.

use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use orbit_cmd::registry_runtime::{
    RegisteredRuntimeFactory, RegisteredRuntimeStamp, ResolvedWorkspaceSelection,
};
use orbit_cmd::task_owner::{self, WorkspaceIdentity};
use orbit_common::protocol::tool_input::required_string;
use orbit_common::{NotFoundKind, OrbitError};
use orbit_core::OrbitRuntime;
use orbit_core::adapter::command::{
    ToolEntryPoint, execute_global_in_process_tool_dispatch, execute_global_plugin_tool,
    host_plugin_mcp_definitions,
};
use orbit_core::runtime::{HostLifetime, resolve_global_root};
use orbit_mcp::federated;
use orbit_mcp::{ListenerExposure, McpHost, McpListener, McpSessionAuthority};
use orbit_types::tool::{McpToolDefinition, McpToolScope, ToolSessionContext};
use orbit_types::workspace::{Workspace, WorkspaceCheckout};
use serde_json::Value;

/// Tools whose target is a machine-global primary key, and therefore whose
/// default binding follows that ID instead of the session [ORB-10797]
/// [ORB-12254]. `orbit.task.artifact.get` reads a payload owned by a task, so
/// it resolves the same way `orbit.task.show` does — its own schema advertises
/// `workspace` as an optional filter, and behavior must agree.
///
/// `pub(crate)` and re-exported through `command::mcp` so the CLI path in
/// `command/tool/run.rs` (and `command/operation.rs`'s task-artifact routing)
/// share this single list instead of keeping a second one that can drift
/// [ORB-12263].
pub(crate) const ID_RESOLVED_WORKSPACE_TOOLS: &[&str] =
    &["orbit.task.show", "orbit.task.artifact.get"];

/// Serve one stdio MCP session.
///
/// This is the entry point an SSH caller reaches, and its `--operator` is a
/// statement there exactly as it is locally: the caller composed that argv over
/// an SSH login to this machine, which is ownership of it, so there is no
/// second authorization for the destination to make [ORB-12564]. A leftover
/// file from the retired destination-side model is named once and ignored.
pub(super) fn serve_mcp_stdio(
    remote_caller_machine_id: Option<String>,
    authority: McpSessionAuthority,
    bound_workspace: Option<String>,
    bound_orchestrator: Option<String>,
) -> Result<(), OrbitError> {
    let global_root = resolve_global_root()?;
    orbit_mcp::warn_ignored_caller_authorization(&global_root);
    let (host, session_context) = compose_server(
        global_root,
        remote_caller_machine_id,
        authority,
        bound_workspace,
        bound_orchestrator,
    )?;
    block_on_server(orbit_mcp::serve_stdio_with_context(host, session_context))
}

/// Serve the federated mux: the accepting machine plus operator-configured
/// SSH remotes, as one stdio surface.
///
/// Local workspaces are an implicit destination and are listed and routed
/// through [`ServerMcpHost`] in-process. Remote membership comes from the
/// machine-global destinations file, whose duplicate-`machine_id` check runs
/// here, before any tool is advertised. A missing or empty file is a valid
/// local-only configuration.
pub(super) fn serve_mcp_federated_stdio(
    bound_orchestrator: Option<String>,
    authority: McpSessionAuthority,
) -> Result<(), OrbitError> {
    let global_root = resolve_global_root()?;
    let remotes = federated::load_destinations(&federated::destinations_path(&global_root))?;
    // The mux is a client to each remote, and identifies itself with the same
    // audit label the v1 proxy forwards. `authority` is one statement serving
    // two roles: the local host stamps it on the sessions it answers directly,
    // and the SSH probe asks each destination for the same thing in its argv
    // [ORB-12564]. Local and remote workspaces therefore behave alike in one
    // namespace, which is the whole point of the mux.
    let mut identity = orbit_mcp::mcp_server_identity(&global_root, None, authority)?;
    // The mux binds no workspace, but it does carry one attribution default.
    // Local destinations read it from this context; remote ones are told in
    // their own argv, because a routed SSH session forwards no context
    // [ORB-11313].
    let bound_orchestrator = normalized_selector(bound_orchestrator);
    identity.session_context.orchestrator = bound_orchestrator.clone();
    identity.session_context.worker_invocation =
        OrbitRuntime::current_worker_invocation(&global_root)?;
    if let Some(binding) = &identity.session_context.worker_invocation {
        identity.session_context.workspace = Some(binding.owner_destination.clone());
        identity
            .session_context
            .effective_capabilities
            .remove(&orbit_types::tool::McpCapability::Operator);
    }
    let destinations = federated::federated_membership(
        identity.process_machine_id.clone(),
        identity.process_machine_name.clone(),
        remotes,
    );
    let local_machine = Arc::new(ServerMcpHost::new(
        global_root,
        identity.process_machine_id.clone(),
        identity.process_machine_name.clone(),
    ));
    // Two budgets, not one: the probe timeout bounds the round trips that
    // decide where a call goes, while the routed `tools/call` is stamped
    // separately at dispatch so a remote run that legitimately takes minutes
    // is not cut short by the time spent classifying its route [ORB-11023].
    let probe = federated::CompositeDestinationProbe::new(
        Arc::new(federated::InProcessDestinationProbe::new(
            local_machine,
            identity.session_context.clone(),
        )),
        Arc::new(federated::SshDestinationProbe::new(
            identity.process_machine_id.clone(),
            federated::DEFAULT_PROBE_TIMEOUT,
            federated::DEFAULT_ROUTED_DELIVERY_TIMEOUT,
            bound_orchestrator,
            authority,
        )),
    );
    let host: Arc<dyn McpHost> = Arc::new(federated::FederatedMcpHost::new(
        destinations,
        Arc::new(probe),
    ));
    tracing::info!(
        machine_id = %identity.process_machine_id,
        "serving the federated MCP mux"
    );
    // Session-unbound by construction: the federated list takes no workspace,
    // and a routed call is addressed only by the copied host-qualified selector.
    block_on_server(orbit_mcp::serve_stdio_with_context(
        host,
        identity.session_context,
    ))
}

pub(super) fn serve_mcp_listener(
    addr: SocketAddr,
    exposure: ListenerExposure,
) -> Result<(), OrbitError> {
    // A listener has no forwarding proxy in front of it, so there is no caller
    // machine label to trust; each accepted connection contributes only the
    // peer address it was observed at.
    //
    // For the same reason the socket serves agent authority only: it
    // authenticates no client, so every accepted connection would otherwise
    // inherit whatever authority the listening process was started with.
    //
    // For the same reason it binds no workspace: a socket is shared by
    // whoever can reach it, so each session names its own workspace.
    //
    // This reasoning is unchanged by argv-propagated remote authority: SSH
    // authenticates the caller before Orbit runs, and a socket authenticates
    // nobody at all [ORB-12564].
    let global_root = resolve_global_root()?;
    let (host, session_context) =
        compose_server(global_root, None, McpSessionAuthority::Agent, None, None)?;
    block_on_server(async move {
        let listener = McpListener::bind(addr, exposure, host, session_context).await?;
        tracing::info!(address = %listener.local_addr()?, "orbit mcp listener bound");
        listener.serve().await
    })
}

/// Build the one MCP host this process serves, together with the trusted
/// session envelope derived from the accepting machine's identity, the
/// authority this process was started with, and the workspace it was launched
/// for.
///
/// `bound_workspace` is the launching configuration's answer to "which
/// workspace is this server for" — the same selector a client could announce
/// at initialize, supplied by whoever wrote the integration because most MCP
/// clients cannot announce anything. A managed child that launches this
/// server without `--workspace` still supplies that selector through the
/// trusted `ORBIT_WORKSPACE` envelope. It is still just a selector: it is
/// resolved against this machine's registry on every call and overridden by an
/// explicit per-call `workspace`.
fn compose_server(
    global_root: PathBuf,
    remote_caller_machine_id: Option<String>,
    authority: McpSessionAuthority,
    bound_workspace: Option<String>,
    bound_orchestrator: Option<String>,
) -> Result<(Arc<dyn McpHost>, ToolSessionContext), OrbitError> {
    let mut identity =
        orbit_mcp::mcp_server_identity(&global_root, remote_caller_machine_id, authority)?;
    identity.session_context.worker_invocation =
        OrbitRuntime::current_worker_invocation(&global_root)?;
    if identity.session_context.worker_invocation.is_some() {
        identity
            .session_context
            .effective_capabilities
            .remove(&orbit_types::tool::McpCapability::Operator);
    }
    identity.session_context.workspace = normalized_selector(bound_workspace);
    identity.session_context.orchestrator = normalized_selector(bound_orchestrator);
    let host = Arc::new(ServerMcpHost::new(
        global_root,
        identity.process_machine_id,
        identity.process_machine_name,
    ));
    Ok((host, identity.session_context))
}

/// Reduce a launch-time selector to the value a session should carry, so an
/// omitted flag and a whitespace-only one are the same absent default.
fn normalized_selector(value: Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn block_on_server<F>(server: F) -> Result<(), OrbitError>
where
    F: Future<Output = Result<(), OrbitError>>,
{
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| OrbitError::Execution(format!("tokio runtime: {error}")))?;
    runtime.block_on(server)
}

/// Tracing target and message emitted once per workspace runtime this process
/// opens.
///
/// Together they are the observable seam for "this server reuses what it
/// opens": the MCP integration test enables this target and counts the lines
/// across calls, so both strings are matched there literally.
const RUNTIME_OPEN_LOG_TARGET: &str = "orbit.mcp.runtime";
const OPENED_WORKSPACE_RUNTIME_LOG: &str = "opened a workspace runtime";

/// The runtimes this process has built, keyed by logical workspace ID.
///
/// [`HostLifetime::LongLived`] promises the host keeps what it opens, but the
/// server used to drop each runtime at the end of the call that built it — so
/// every workspace-scoped call re-parsed the host identity, reopened the task
/// registry and all workspace stores, and started a fresh embed worker. An
/// entry is reused only while everything it was composed from still holds: the
/// registry records this call resolved, plus a [`RegisteredRuntimeStamp`] over
/// the files behind them and over both `config.toml` layers, so an edit to the
/// runtime configuration takes effect on the next call rather than at the next
/// server restart.
///
/// Generic over the cached value so the unit tests can exercise reuse and
/// invalidation without opening real stores.
struct WorkspaceRuntimeCache<T = OrbitRuntime> {
    entries: Mutex<HashMap<String, CachedRuntime<T>>>,
}

/// One built runtime together with the facts it was composed from.
struct CachedRuntime<T> {
    workspace: Workspace,
    checkout: WorkspaceCheckout,
    stamp: RegisteredRuntimeStamp,
    value: Arc<T>,
}

impl<T> CachedRuntime<T> {
    /// `ResolvedWorkspaceSelection::local_root` is deliberately not compared:
    /// [`RegisteredRuntimeFactory::open_registered_checkout_for`] composes
    /// against the registered checkout's own `.orbit` for both roots, so a
    /// selection that differs only there yields the same runtime.
    fn matches(
        &self,
        selected: &ResolvedWorkspaceSelection,
        stamp: &RegisteredRuntimeStamp,
    ) -> bool {
        self.workspace == selected.workspace
            && self.checkout == selected.checkout
            && self.stamp == *stamp
    }
}

impl<T> Default for WorkspaceRuntimeCache<T> {
    fn default() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }
}

impl<T> WorkspaceRuntimeCache<T> {
    /// Reuse the runtime already built for `selected`, or build one and keep it.
    ///
    /// `build` runs outside the cache lock: opening stores blocks on I/O, and
    /// two concurrent builds for the same facts are interchangeable.
    fn resolve(
        &self,
        global_root: &Path,
        selected: &ResolvedWorkspaceSelection,
        build: impl FnOnce() -> Result<T, OrbitError>,
    ) -> Result<Arc<T>, OrbitError> {
        let stamp = RegisteredRuntimeStamp::read(global_root, &selected.checkout);
        if let Some(value) = self.reusable(selected, &stamp) {
            return Ok(value);
        }
        let value = Arc::new(build()?);
        let mut entries = self.lock();
        // A racing call may have published an equivalent entry while this one
        // built; prefer the published runtime so the session converges on one.
        if let Some(published) = entries
            .get(&selected.workspace.id)
            .filter(|cached| cached.matches(selected, &stamp))
        {
            return Ok(Arc::clone(&published.value));
        }
        // Otherwise this build becomes the entry, replacing whatever stale one
        // a rebind or an edited registry left behind for this workspace.
        entries.insert(
            selected.workspace.id.clone(),
            CachedRuntime {
                workspace: selected.workspace.clone(),
                checkout: selected.checkout.clone(),
                stamp,
                value: Arc::clone(&value),
            },
        );
        Ok(value)
    }

    /// The cached runtime for this selection iff every fact it was built from
    /// is unchanged. A mismatch reports absent, so the caller rebuilds.
    fn reusable(
        &self,
        selected: &ResolvedWorkspaceSelection,
        stamp: &RegisteredRuntimeStamp,
    ) -> Option<Arc<T>> {
        self.lock()
            .get(&selected.workspace.id)
            .filter(|cached| cached.matches(selected, stamp))
            .map(|cached| Arc::clone(&cached.value))
    }

    /// Poisoning is recoverable here: the map is an idempotent build cache, so
    /// a panic in another call cannot leave it logically inconsistent.
    fn lock(&self) -> MutexGuard<'_, HashMap<String, CachedRuntime<T>>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// One MCP server bound to the executing machine.
struct ServerMcpHost {
    global_root: PathBuf,
    process_machine_id: String,
    process_machine_name: String,
    /// Runtimes this long-lived host has already opened.
    workspace_runtimes: WorkspaceRuntimeCache,
}

impl ServerMcpHost {
    fn new(global_root: PathBuf, process_machine_id: String, process_machine_name: String) -> Self {
        Self {
            global_root,
            process_machine_id,
            process_machine_name,
            workspace_runtimes: WorkspaceRuntimeCache::default(),
        }
    }

    fn definition(&self, name: &str) -> Result<McpToolDefinition, OrbitError> {
        self.advertised_definitions()?
            .into_iter()
            .find(|definition| definition.schema.name == name)
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::Tool, name.to_string()))
    }

    /// The canonical built-in surface plus this host's active plugin tools.
    ///
    /// The built-in half is memoised process-wide because it is a function of
    /// the binary alone. The plugin half is not: it changes with `orbit plugin
    /// add|enable|disable|remove`, so it is read from the host's plugin
    /// records on each call rather than frozen at first use.
    fn advertised_definitions(&self) -> Result<Vec<McpToolDefinition>, OrbitError> {
        let mut definitions = orbit_mcp::canonical_mcp_tool_definitions()
            .map_err(|error| OrbitError::InvalidInput(error.to_string()))?;
        match host_plugin_mcp_definitions(&self.global_root) {
            Ok(plugins) => definitions.extend(plugins),
            // A plugin problem is that plugin's problem: the built-in surface
            // must still be listable (design §4.9).
            Err(error) => tracing::warn!(
                target: "orbit.mcp.plugin",
                error = %error,
                "omitting plugin tools from tools/list"
            ),
        }
        definitions.sort_by(|left, right| left.schema.name.cmp(&right.schema.name));
        orbit_types::tool::validate_mcp_tool_definitions(&definitions)
            .map_err(|error| OrbitError::InvalidInput(error.to_string()))?;
        Ok(definitions)
    }

    /// Whether `name` is one of this host's plugin tools.
    fn is_plugin_tool(&self, name: &str) -> bool {
        host_plugin_mcp_definitions(&self.global_root).is_ok_and(|definitions| {
            definitions
                .iter()
                .any(|definition| definition.schema.name == name)
        })
    }

    fn workspace_selector<'a>(
        input: &'a Value,
        context: &'a ToolSessionContext,
    ) -> Option<&'a str> {
        call_workspace_selector(input)
            .or(context.workspace.as_deref())
            .map(str::trim)
            .filter(|selector| !selector.is_empty())
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

    fn list_workspaces(&self) -> Result<Value, OrbitError> {
        let registry_path =
            orbit_registry::workspace_registry::registry_path_for(&self.global_root);
        let registry = orbit_registry::workspace_registry::load_registry_from(&registry_path)?;
        orbit_mcp::execute_discovery_tool(
            "orbit.workspace.list",
            &registry,
            &self.process_machine_id,
        )
    }

    fn list_federated_workspaces(&self) -> Result<Value, OrbitError> {
        let registry_path =
            orbit_registry::workspace_registry::registry_path_for(&self.global_root);
        let registry = orbit_registry::workspace_registry::load_registry_from(&registry_path)?;
        Ok(orbit_mcp::execute_federated_workspace_discovery(
            &registry,
            &self.process_machine_id,
        ))
    }

    fn call_global_tool(
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
            |_| match name {
                "orbit.workspace.list" => self.list_workspaces(),
                orbit_mcp::FEDERATED_DESTINATION_WORKSPACE_LIST_TOOL => {
                    self.list_federated_workspaces()
                }
                _ => Err(OrbitError::not_found(NotFoundKind::Tool, name.to_string())),
            },
        )
        .map(|outcome| outcome.value)
    }

    /// A `mcp_scope: global` plugin tool: no workspace to open, so it runs
    /// against the host's plugin records inside Core's audited dispatch.
    fn call_global_plugin_tool(
        &self,
        name: &str,
        input: Value,
        context: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        execute_global_plugin_tool(&self.global_root, name, input, ToolEntryPoint::Mcp, context)
    }

    fn resolve_workspace_runtime(
        &self,
        name: &str,
        input: &Value,
        context: &ToolSessionContext,
    ) -> Result<(Arc<OrbitRuntime>, ResolvedWorkspaceSelection), OrbitError> {
        let selected = self.workspace_selection(name, input, context)?;
        let runtime = self
            .workspace_runtimes
            .resolve(&self.global_root, &selected, || {
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
            })?;
        Ok((runtime, selected))
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
    fn workspace_selection(
        &self,
        name: &str,
        input: &Value,
        context: &ToolSessionContext,
    ) -> Result<ResolvedWorkspaceSelection, OrbitError> {
        if ID_RESOLVED_WORKSPACE_TOOLS.contains(&name) && call_workspace_selector(input).is_none() {
            let task_id = required_string(input, &["id"], "id")?;
            return task_owner::resolve_task_owner(&self.global_root, &task_id);
        }
        let selector = Self::workspace_selector(input, context)
            .ok_or_else(|| self.workspace_required(name))?;
        RegisteredRuntimeFactory::resolve_workspace_selector(&self.global_root, selector)
    }

    fn audit_global_failure(
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

    fn call_workspace_tool(
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
                input = orbit_cmd::prepare_remote_task_artifact_put(input, Some(&cwd), Some(&cwd))?;
                name
            } else {
                name
            };
            if let Some(object) = input.as_object_mut() {
                if object.get("workspace").is_some_and(|value| {
                    value.as_str() != Some(&binding.owner_destination)
                        && value.as_str() != Some(&binding.owner_workspace_id)
                        && !value.as_str().is_some_and(|selector| {
                            std::env::current_dir().ok().is_some_and(|cwd| {
                                std::fs::canonicalize(selector).is_ok_and(|path| path == cwd)
                            })
                        })
                }) {
                    return Err(OrbitError::PolicyDenied(
                        "worker workspace binding mismatch".into(),
                    ));
                }
                object.insert(
                    "workspace".into(),
                    Value::String(binding.owner_destination.clone()),
                );
            }
            let remotes =
                federated::load_destinations(&federated::destinations_path(&self.global_root))?;
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

        if name == "orbit.crew.list" {
            let workspace_id = selected.workspace.id.clone();
            let owner_machine_id = selected.workspace.owner_machine_id.clone();
            let crew_runtime = &runtime;
            return runtime
                .execute_in_process_tool_dispatch(
                    name,
                    input,
                    ToolEntryPoint::Mcp,
                    context,
                    move |_| {
                        serde_json::to_value(
                            crew_runtime.crew_discovery(&workspace_id, owner_machine_id)?,
                        )
                        .map_err(|error| {
                            OrbitError::Execution(format!("serialize crew discovery: {error}"))
                        })
                    },
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

impl McpHost for ServerMcpHost {
    fn list_mcp_tool_definitions(&self) -> Result<Vec<McpToolDefinition>, OrbitError> {
        self.advertised_definitions()
    }

    fn friction_tag_taxonomy(
        &self,
        context: &ToolSessionContext,
    ) -> Result<Option<Vec<(String, String)>>, OrbitError> {
        if context.workspace.is_none() {
            return Ok(None);
        }
        let input = Value::Object(Default::default());
        // Schema decoration is advisory: a session hint that does not resolve
        // (an unregistered runtime identity, a foreign path) advertises the
        // shipped defaults here and fails closed at call time, so tools/list
        // stays available for globally resolved tools such as task.show.
        let Ok((runtime, _selected)) =
            self.resolve_workspace_runtime("orbit.friction.add", &input, context)
        else {
            return Ok(None);
        };
        runtime.friction_tag_taxonomy().map(Some)
    }

    fn call_tool(
        &self,
        name: &str,
        input: Value,
        context: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        // The mux's destination-side discovery path is intentionally absent
        // from tools/list. It retains Invalid local checkouts for descriptor
        // health without changing direct v1 orbit.workspace.list behavior.
        if name == orbit_mcp::FEDERATED_DESTINATION_WORKSPACE_LIST_TOOL {
            return self.call_global_tool(name, input, context);
        }
        let definition = match self.definition(name) {
            Ok(definition) => definition,
            Err(error) => return self.audit_global_failure(name, input, context, error),
        };
        if definition.scope == McpToolScope::Global {
            if self.is_plugin_tool(name) {
                return self.call_global_plugin_tool(name, input, context);
            }
            return self.call_global_tool(name, input, context);
        }
        self.call_workspace_tool(name, input, context)
    }
}

/// The selector the call itself passed, untrimmed. Distinguishing "the caller
/// named a workspace" from "the session announced one" is what makes an
/// explicit selector a filter and the ambient one a default.
fn call_workspace_selector(input: &Value) -> Option<&str> {
    input.get("workspace").and_then(Value::as_str)
}

fn execute_core_tool(
    runtime: &OrbitRuntime,
    name: &str,
    input: Value,
    context: ToolSessionContext,
    owner: Option<&WorkspaceIdentity>,
) -> Result<Value, OrbitError> {
    let output = runtime
        .execute_tool_command_dispatch_with_session_context(
            name,
            input.clone(),
            None,
            None,
            ToolEntryPoint::Mcp,
            context,
        )?
        .value;
    crate::command::task::show::attach_bound_workspace_identity(name, &input, owner, output)
}

#[cfg(test)]
#[path = "tests/server.rs"]
mod tests;
