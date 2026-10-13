//! MCP transports and server composition.

use std::future::Future;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use orbit_common::OrbitError;
use orbit_core::OrbitRuntime;
use orbit_core::runtime::resolve_global_root;
use orbit_mcp::federated;
use orbit_mcp::{ListenerExposure, McpHost, McpListener, McpSessionAuthority};
use orbit_types::tool::ToolSessionContext;

use super::host::ServerMcpHost;

/// Tools whose target is a machine-global primary key, and therefore whose
/// default binding follows that ID instead of the session [ORB-10797]
/// [ORB-12254]. `orbit.task.artifact.get` reads a payload owned by a task, so
/// it resolves the same way `orbit.task.show` does — its own schema advertises
/// `workspace` as an optional filter, and behavior must agree.
///
/// `pub(crate)` and re-exported through `command::mcp` so the CLI path in
/// `command/tool/run.rs`, `command/operation/registry.rs`'s task-artifact
/// routing and `command/operation/spec.rs`'s registry-derived task verbs share
/// this single list instead of keeping a second one that can drift
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
pub(in super::super) fn serve_mcp_stdio(
    remote_caller_machine_id: Option<String>,
    authority: McpSessionAuthority,
    bound_workspace: Option<String>,
    bound_orchestrator: Option<String>,
    internal_drain: bool,
    worker_host: bool,
) -> Result<(), OrbitError> {
    let global_root = resolve_global_root()?;
    orbit_mcp::warn_ignored_caller_authorization(&global_root);
    let (host, mut session_context) = compose_server(
        global_root,
        remote_caller_machine_id,
        authority,
        bound_workspace,
        bound_orchestrator,
    )?;
    // This is launch provenance, never initialize or per-call metadata.
    session_context.worker_host_call = worker_host;
    let exit = if internal_drain {
        block_on_server(orbit_mcp::serve_internal_drain_stdio(host, session_context))?
    } else {
        block_on_server(orbit_mcp::serve_stdio_with_context(host, session_context))?
    };
    finish_stdio_session(exit)
}

/// Serve the federated mux: the accepting machine plus operator-configured
/// SSH remotes, as one stdio surface.
///
/// Local workspaces are an implicit destination and are listed and routed
/// through [`ServerMcpHost`] in-process. Remote membership comes from the
/// machine-global host file (`orbit host`), whose validation runs here,
/// before any tool is advertised. A missing or empty file is a valid
/// local-only configuration.
pub(in super::super) fn serve_mcp_federated_stdio(
    bound_orchestrator: Option<String>,
    authority: McpSessionAuthority,
) -> Result<(), OrbitError> {
    let global_root = resolve_global_root()?;
    let remotes = federated::load_destinations(&global_root)?;
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
    let global_root_for_bridge = global_root.clone();
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
    // An id-only task call goes to the host its prefix names [ORB-14449].
    // The table is the same host file membership came from.
    let task_prefixes = orbit_registry::hosts::load_task_prefix_table(&global_root_for_bridge)?;
    let mirror_root = global_root_for_bridge.clone();
    let host: Arc<dyn McpHost> = Arc::new(
        federated::FederatedMcpHost::new(destinations, Arc::new(probe))
            .with_task_prefix_routing(task_prefixes)
            .with_local_mirror_hint(Arc::new(move |task_id| {
                orbit_cmd::hosts::local_mirror_workspace(&mirror_root, task_id)
            })),
    );
    let host = with_claimed_owner_bridge(
        host,
        &identity.session_context,
        &global_root_for_bridge,
        &identity.process_machine_id,
    );
    tracing::info!(
        machine_id = %identity.process_machine_id,
        "serving the federated MCP mux"
    );
    // Session-unbound by construction: the federated list takes no workspace,
    // and a routed call is addressed only by the copied host-qualified selector.
    finish_stdio_session(block_on_server(orbit_mcp::serve_stdio_with_context(
        host,
        identity.session_context,
    ))?)
}

/// Complete a stdio session: re-exec this command under the installed
/// executable when the session was handed over to it. `exec` keeps the pid
/// and stdio descriptors, so the client keeps its session; every Orbit lock
/// is close-on-exec, so this image's participation ends with the exec.
fn finish_stdio_session(exit: orbit_mcp::StdioExit) -> Result<(), OrbitError> {
    match exit {
        orbit_mcp::StdioExit::Closed | orbit_mcp::StdioExit::Yielded => Ok(()),
        orbit_mcp::StdioExit::HandOver { executable, resume } => {
            let args = std::env::args_os().skip(1).collect::<Vec<_>>();
            Err(orbit_common::fs::generation::reexec(
                &executable,
                &args,
                &[(orbit_mcp::RESUME_ENV, std::ffi::OsStr::new(&resume))],
            ))
        }
    }
}

pub(in super::super) fn serve_mcp_listener(
    addr: SocketAddr,
    exposure: ListenerExposure,
    bound_workspace: Option<String>,
) -> Result<(), OrbitError> {
    // A listener has no forwarding proxy in front of it, so there is no caller
    // machine label to trust; each accepted connection contributes only the
    // peer address it was observed at.
    //
    // For the same reason the socket serves agent authority only: it
    // authenticates no client, so every accepted connection would otherwise
    // inherit whatever authority the listening process was started with.
    //
    // It binds a workspace only when the operator names one (`orbit --workspace
    // <selector> mcp listen`). That is a default and never authority: it is
    // resolved against the registry per call, a client that announces a
    // workspace or passes one per call takes precedence, and any peer could
    // already name the same workspace itself. Without it a socket is shared by
    // whoever can reach it, so each session names its own workspace, and the
    // managed-child `ORBIT_WORKSPACE` envelope is not read here.
    //
    // This reasoning is unchanged by argv-propagated remote authority: SSH
    // authenticates the caller before Orbit runs, and a socket authenticates
    // nobody at all [ORB-12564].
    let global_root = resolve_global_root()?;
    let (host, session_context) = compose_server(
        global_root,
        None,
        McpSessionAuthority::Agent,
        bound_workspace,
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
    let host: Arc<dyn McpHost> = Arc::new(ServerMcpHost::new(
        global_root.clone(),
        identity.process_machine_id.clone(),
        identity.process_machine_name,
    ));
    let host = with_claimed_owner_bridge(
        host,
        &identity.session_context,
        &global_root,
        &identity.process_machine_id,
    );
    Ok((host, identity.session_context))
}

/// A managed worker's host, with a claimed worker's owner calls routed
/// through its run's broker as its CLI routes them.
fn with_claimed_owner_bridge(
    host: Arc<dyn McpHost>,
    session_context: &ToolSessionContext,
    global_root: &Path,
    process_machine_id: &str,
) -> Arc<dyn McpHost> {
    #[cfg(unix)]
    if session_context.worker_invocation.is_some() {
        return Arc::new(super::super::claimed_owner::ClaimedOwnerBridge {
            inner: host,
            global_root: global_root.to_path_buf(),
            process_machine_id: process_machine_id.to_string(),
        });
    }
    #[cfg(not(unix))]
    let _ = (session_context, global_root, process_machine_id);
    host
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

fn block_on_server<F, T>(server: F) -> Result<T, OrbitError>
where
    F: Future<Output = Result<T, OrbitError>>,
{
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| OrbitError::Execution(format!("tokio runtime: {error}")))?;
    runtime.block_on(server)
}
