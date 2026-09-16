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
use orbit_core::adapter::command::{ToolEntryPoint, execute_global_in_process_tool_dispatch};
use orbit_core::runtime::{HostLifetime, resolve_global_root};
use orbit_mcp::federated;
use orbit_mcp::{
    ListenerExposure, McpHost, McpListener, McpSessionAuthority, SessionCapabilityPolicy,
    SshAcceptance,
};
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
/// This is the only entry point whose authority is resolved against the
/// destination's callers file: it is the one an SSH caller reaches, and
/// therefore the one whose `--operator` is a request rather than a statement
/// [ORB-11052]. `acceptance` is how this machine's own argv describes the
/// session's arrival — a forced command it wrote itself, or nothing, in which
/// case the destination falls back to observing its environment [ORB-11053].
pub(super) fn serve_mcp_stdio(
    remote_caller_machine_id: Option<String>,
    authority: McpSessionAuthority,
    bound_workspace: Option<String>,
    acceptance: SshAcceptance,
    bound_orchestrator: Option<String>,
) -> Result<(), OrbitError> {
    let global_root = resolve_global_root()?;
    let policy = orbit_mcp::mcp_serve_session_policy(
        &global_root,
        remote_caller_machine_id.as_deref(),
        authority,
        &acceptance,
    )?;
    let (host, session_context) = compose_server(
        global_root,
        remote_caller_machine_id,
        policy,
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
) -> Result<(), OrbitError> {
    let global_root = resolve_global_root()?;
    let remotes = federated::load_destinations(&federated::destinations_path(&global_root))?;
    // The mux is a client to each remote, and identifies itself with the same
    // audit label the v1 proxy forwards. Local calls reuse this process's
    // identity and authority rather than opening SSH. Being a client is also
    // why it composes a local policy: each destination caps the mux
    // independently with its own callers file, and the mux is not a
    // destination for its own request [ORB-11052].
    let mut identity = orbit_mcp::mcp_server_identity(
        &global_root,
        None,
        &SessionCapabilityPolicy::local(McpSessionAuthority::Agent),
    )?;
    // The mux binds no workspace, but it does carry one attribution default.
    // Local destinations read it from this context; remote ones are told in
    // their own argv, because a routed SSH session forwards no context
    // [ORB-11313].
    let bound_orchestrator = normalized_selector(bound_orchestrator);
    identity.session_context.orchestrator = bound_orchestrator.clone();
    let destinations = federated::federated_membership(
        identity.process_machine_id.clone(),
        identity.process_host_id.clone(),
        remotes,
    );
    let local_host = Arc::new(ServerMcpHost::new(
        global_root,
        identity.process_machine_id.clone(),
        identity.process_host_id.clone(),
        SessionCapabilityPolicy::local(McpSessionAuthority::Agent),
    ));
    // Two budgets, not one: the probe timeout bounds the round trips that
    // decide where a call goes, while the routed `tools/call` is stamped
    // separately at dispatch so a remote run that legitimately takes minutes
    // is not cut short by the time spent classifying its route [ORB-11023].
    let probe = federated::CompositeDestinationProbe::new(
        Arc::new(federated::InProcessDestinationProbe::new(
            local_host,
            identity.session_context.clone(),
        )),
        Arc::new(federated::SshDestinationProbe::new(
            identity.process_machine_id.clone(),
            federated::DEFAULT_PROBE_TIMEOUT,
            federated::DEFAULT_ROUTED_DELIVERY_TIMEOUT,
            bound_orchestrator,
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
    // A callers file is a statement about SSH callers this machine serves
    // directly; a socket peer is not one of those, so the listener composes a
    // local policy and its hardcoded agent authority is unchanged.
    let global_root = resolve_global_root()?;
    let (host, session_context) = compose_server(
        global_root,
        None,
        SessionCapabilityPolicy::local(McpSessionAuthority::Agent),
        None,
        None,
    )?;
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
    policy: SessionCapabilityPolicy,
    bound_workspace: Option<String>,
    bound_orchestrator: Option<String>,
) -> Result<(Arc<dyn McpHost>, ToolSessionContext), OrbitError> {
    let mut identity =
        orbit_mcp::mcp_server_identity(&global_root, remote_caller_machine_id, &policy)?;
    identity.session_context.workspace = normalized_selector(bound_workspace);
    identity.session_context.orchestrator = normalized_selector(bound_orchestrator);
    let host = Arc::new(ServerMcpHost::new(
        global_root,
        identity.process_machine_id,
        identity.process_host_id,
        policy,
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
/// the files behind them.
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
    process_host_id: String,
    /// What this session may do here, kept for the per-call re-evaluation a
    /// `workspaces` narrowing needs [ORB-11052].
    session_policy: SessionCapabilityPolicy,
    /// Runtimes this long-lived host has already opened.
    workspace_runtimes: WorkspaceRuntimeCache,
}

impl ServerMcpHost {
    fn new(
        global_root: PathBuf,
        process_machine_id: String,
        process_host_id: String,
        session_policy: SessionCapabilityPolicy,
    ) -> Self {
        Self {
            global_root,
            process_machine_id,
            process_host_id,
            session_policy,
            workspace_runtimes: WorkspaceRuntimeCache::default(),
        }
    }

    fn definition(&self, name: &str) -> Result<McpToolDefinition, OrbitError> {
        orbit_mcp::canonical_mcp_tool_definitions()
            .map_err(|error| OrbitError::InvalidInput(error.to_string()))?
            .into_iter()
            .find(|definition| definition.schema.name == name)
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::Tool, name.to_string()))
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
        context.process_host_id = Some(self.process_host_id.clone());
        // The destination now knows which registered workspace this call lands
        // in, which is the only point a `workspaces` narrowing can be decided
        // against. A local session's policy holds no grant and re-stamps the
        // same capabilities it was established with [ORB-11052].
        self.session_policy
            .stamp(&mut context, Some(&selected.workspace.id));

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
        orbit_mcp::canonical_mcp_tool_definitions()
            .map_err(|error| OrbitError::InvalidInput(error.to_string()))
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
