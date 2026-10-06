//! A claimed worker's owner coordination through its run's broker [ORB-14260].
//!
//! A claimed worker on another machine than its owner reaches the owner over
//! SSH, and the agent sandbox masks `~/.ssh`, so the nested `orbit` (CLI or
//! MCP) cannot open that route itself. With `ORBIT_PLUGIN_BROKER` set it hands
//! the closed [`CLAIMED_OWNER_TOOLS`] to the broker in the unconfined step
//! runner, which already holds the claim's owner route. Inside a masked
//! sandbox it refuses every other coordination tool rather than attempting an
//! SSH route that can only fail, and reports a missing or unreachable broker
//! as `owner_route_unavailable`: no repair from the same sandbox can open the
//! route, so the run skips step and final recovery and releases the claim.
//!
//! The broker decides scope from its own records. The worker binding names
//! the task, claim, owner and leaf run; the run's dispatch record names the
//! task and activity. The request contributes only the call's own fields,
//! each checked against that scope before anything reaches the owner:
//!
//! - `orbit.task.show` and the artifact calls name the claimed task;
//! - `orbit.task.add` names the claimed task, and only it, as `spawned_from`;
//! - `orbit.friction.add` is recorded during the claimed task.
//!
//! The before-PR gate's own artifacts keep the reviewer's attempt scope
//! ([`super::claimed_review`]). The broker never opens a path the agent named:
//! the nested `orbit` reads an artifact source inside the sandbox, no-follow,
//! under the workspace confinement `orbit.task.artifact.put` already applies,
//! and sends the bytes.

use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use orbit_common::OrbitError;
use orbit_types::task::{MAX_TASK_ARTIFACT_CONTENT_BYTES, media_type_for_artifact_path};
use orbit_types::tool::{
    CLAIMED_OWNER_TOOLS, ToolSessionContext, WorkerInvocation, is_claimed_owner_tool,
};
use orbit_types::workflow::{OWNER_ROUTE_UNAVAILABLE_ERROR_CODE, OWNER_ROUTE_UNAVAILABLE_MARKER};
use serde_json::{Map, Value, json};

use super::execute::ToolEntryPoint;
use crate::OrbitRuntime;

pub(super) const GET: &str = "orbit.task.artifact.get";
pub(super) const PUT: &str = "orbit.task.artifact.put";
const TASK_SHOW: &str = "orbit.task.show";
const TASK_ADD: &str = "orbit.task.add";
const FRICTION_ADD: &str = "orbit.friction.add";

/// Whether this process is a claimed worker whose owner is another machine:
/// the only caller whose coordination calls need the bridge. A worker on its
/// owner's machine reaches the owner store in process and keeps that route.
pub(super) fn remote_owner<'a>(
    binding: Option<&'a WorkerInvocation>,
    process_machine_id: Option<&str>,
) -> Option<&'a WorkerInvocation> {
    let binding = binding?;
    let local = process_machine_id?;
    (binding.execution.machine_id == local && binding.owner_machine_id != local).then_some(binding)
}

/// How a claimed worker's coordination call leaves the nested `orbit`.
#[derive(Debug)]
pub enum ClaimedOwnerRoute {
    /// The broker answered. The broker records the call's audit row.
    Forwarded(Result<Value, OrbitError>),
    /// Refused before a valid broker response: a tool the broker does not
    /// carry, no broker inside a masked sandbox, a request the nested process
    /// could not prepare, or an unavailable/invalid transport response.
    /// Nothing else can be relied on to have recorded it, so the caller
    /// audits the refusal.
    Refused(OrbitError),
}

/// Route a claimed worker's coordination call from the nested `orbit` (CLI
/// or MCP) to its run's broker.
///
/// `None` leaves the call on the caller's existing route: not a coordination
/// tool, no remote owner, or an unsandboxed worker without a broker, which
/// can reach the owner itself.
#[allow(clippy::too_many_arguments)]
pub fn bridge_claimed_owner_call(
    global_root: &Path,
    binding: Option<&WorkerInvocation>,
    process_machine_id: Option<&str>,
    name: &str,
    input: &Value,
    cwd: &Path,
    workspace_root: &Path,
    entry_point: ToolEntryPoint,
) -> Option<ClaimedOwnerRoute> {
    let socket = std::env::var_os("ORBIT_PLUGIN_BROKER").map(std::path::PathBuf::from);
    bridge_through(
        socket.as_deref(),
        global_root,
        binding,
        process_machine_id,
        name,
        input,
        cwd,
        workspace_root,
        entry_point,
    )
}

/// [`bridge_claimed_owner_call`] with the broker socket, if any, given.
#[allow(clippy::too_many_arguments)]
pub(super) fn bridge_through(
    socket: Option<&Path>,
    global_root: &Path,
    binding: Option<&WorkerInvocation>,
    process_machine_id: Option<&str>,
    name: &str,
    input: &Value,
    cwd: &Path,
    workspace_root: &Path,
    entry_point: ToolEntryPoint,
) -> Option<ClaimedOwnerRoute> {
    if !crate::runtime::is_coordination_tool(name) {
        return None;
    }
    let binding = binding?;
    let local = process_machine_id?;
    // A local owner does not need the broker, but the newly granted read
    // still has to stay within this claim. Reject the internal projections
    // here too: they are reserved for host-owned reads and can enumerate
    // records beyond the claimed task.
    if binding.execution.machine_id == local
        && binding.owner_machine_id == local
        && name == TASK_SHOW
    {
        if input.get("_worker_read").is_some() {
            return Some(ClaimedOwnerRoute::Refused(denied(
                "internal task projections are not available to a claimed worker",
            )));
        }
        if input.get("id").and_then(Value::as_str) != Some(binding.task_id.as_str()) {
            return Some(ClaimedOwnerRoute::Refused(denied(
                "the request names a task other than the claimed task",
            )));
        }
        return None;
    }
    let binding = remote_owner(Some(binding), Some(local))?;
    let masked = || crate::runtime::plugin::sandbox_mask::plugin_trees_masked(global_root);
    if !is_claimed_owner_tool(name) {
        return masked().then(|| ClaimedOwnerRoute::Refused(outside_allowlist(name)));
    }
    let Some(socket) = socket else {
        return masked().then(|| {
            ClaimedOwnerRoute::Refused(owner_route_unavailable(
                name,
                "ORBIT_PLUGIN_BROKER is not set in this process",
            ))
        });
    };
    let request = match bridged_input(binding, name, input, cwd, workspace_root) {
        Ok(request) => request,
        Err(error) => return Some(ClaimedOwnerRoute::Refused(error)),
    };
    let result = crate::runtime::plugin::broker::forward_call_with_status(
        socket,
        name,
        request,
        cwd,
        None,
        match entry_point {
            ToolEntryPoint::Cli => "cli",
            ToolEntryPoint::Mcp => "mcp",
        },
    );
    Some(match result {
        Ok(value) => ClaimedOwnerRoute::Forwarded(Ok(value)),
        Err(crate::runtime::plugin::broker::ForwardCallError::BrokerAudit(error)) => {
            ClaimedOwnerRoute::Forwarded(Err(error))
        }
        Err(crate::runtime::plugin::broker::ForwardCallError::CallerAudit(error)) => {
            ClaimedOwnerRoute::Refused(coordinator_unavailable(name, error))
        }
    })
}

/// The typed failure a claimed worker reports when its coordinator cannot
/// carry the call. The agent ends its step on it; the run skips recovery and
/// releases the claim, since nothing inside this sandbox can open the route.
pub fn owner_route_unavailable(name: &str, cause: &str) -> OrbitError {
    let message = format!(
        "{OWNER_ROUTE_UNAVAILABLE_MARKER} '{name}' for a claimed task reaches the owner only \
         through this run's coordinator, and {cause}. The agent sandbox masks ~/.ssh, so there \
         is no other route: do not read SSH credentials or loosen the sandbox. End the step as \
         failed with error.code `{OWNER_ROUTE_UNAVAILABLE_ERROR_CODE}`; the run then releases \
         the claim for a retry"
    );
    OrbitError::RemoteTool {
        code: OWNER_ROUTE_UNAVAILABLE_ERROR_CODE.to_string(),
        payload: json!({
            "code": OWNER_ROUTE_UNAVAILABLE_ERROR_CODE,
            "message": message,
            "retryable": false,
        }),
        message,
    }
}

/// An unreachable broker means the run's step runner is gone, so the call
/// cannot be retried from inside the sandbox. Other caller-side refusals —
/// a busy listener, a request over its limit — keep their own code.
fn coordinator_unavailable(name: &str, error: OrbitError) -> OrbitError {
    match error {
        OrbitError::RemoteTool { code, message, .. } if code == "plugin_broker_unavailable" => {
            owner_route_unavailable(
                name,
                &format!(
                    "the call could not reach this run's coordinator ({message}); the step \
                     runner that carries a claimed worker's owner calls has stopped or is \
                     restarting"
                ),
            )
        }
        other => other,
    }
}

/// Refuse a coordination tool the broker does not carry, from inside a
/// masked sandbox: the SSH route to the owner could only fail there.
fn outside_allowlist(name: &str) -> OrbitError {
    OrbitError::CapabilityDenied(format!(
        "claimed_owner_bridge_refused: '{name}' is not among the owner calls a claimed worker's \
         sandbox can make ({}). The sandbox masks ~/.ssh, and this run's coordinator carries \
         only those; return the work through the step's output fields instead",
        CLAIMED_OWNER_TOOLS.join(", ")
    ))
}

/// The request the broker receives. A `workspace` may only confirm the
/// binding; it is dropped, since the broker routes to the claim's owner
/// regardless, as is the MCP envelope's `_meta`, which describes this process
/// rather than the call. A put carries the bytes read here, base64-encoded so
/// a full-size artifact fits the broker's frame, never the source path.
fn bridged_input(
    binding: &WorkerInvocation,
    name: &str,
    input: &Value,
    cwd: &Path,
    workspace_root: &Path,
) -> Result<Value, OrbitError> {
    let mut input = input.clone();
    if let Some(object) = input.as_object_mut() {
        object.remove("_meta");
        if let Some(workspace) = object.remove("workspace") {
            let confirms = workspace.as_str().is_some_and(|selector| {
                selector == binding.owner_destination
                    || selector == binding.owner_workspace_id
                    || std::fs::canonicalize(selector).is_ok_and(|path| {
                        [workspace_root, cwd]
                            .iter()
                            .any(|root| std::fs::canonicalize(root).is_ok_and(|root| root == path))
                    })
            });
            if !confirms {
                return Err(OrbitError::PolicyDenied(
                    "worker workspace binding mismatch".into(),
                ));
            }
        }
    }
    if name != PUT {
        return Ok(input);
    }
    let prepared =
        orbit_tools::prepare_remote_task_artifact_put(input, Some(cwd), Some(workspace_root))?;
    let artifact = prepared["artifacts"]
        .get(0)
        .cloned()
        .ok_or_else(|| OrbitError::Execution("artifact put prepared no payload".into()))?;
    let content: Vec<u8> = serde_json::from_value(artifact["content"].clone())
        .map_err(|error| OrbitError::Execution(format!("artifact put payload: {error}")))?;
    let mut request = json!({
        "id": prepared["id"],
        "path": artifact["path"],
        "content_base64": BASE64_STANDARD.encode(content),
    });
    if let Some(model) = prepared.get("model") {
        request["model"] = model.clone();
    }
    Ok(request)
}

/// Refuse a request in the broker's words, before it reaches the owner.
pub(super) fn denied(reason: &str) -> OrbitError {
    OrbitError::PolicyDenied(format!("claimed_owner_bridge_refused: {reason}"))
}

/// The claim a broker serves, from host records alone: the step runner's
/// worker binding, whose owner must be another machine, and the run's task.
fn claim_binding<'a>(
    runtime: &'a OrbitRuntime,
    run: &orbit_engine::PluginBrokerRun,
) -> Result<&'a WorkerInvocation, OrbitError> {
    let binding = runtime
        .worker_invocation()
        .ok_or_else(|| denied("this run is not bound to a claim"))?;
    if remote_owner(Some(binding), runtime.automation_machine_identity()).is_none() {
        return Err(denied(
            "the claim's owner is this machine, which the worker reaches directly",
        ));
    }
    if run.job_run_id.is_none() {
        return Err(denied("the run has no job-run authority"));
    }
    if run.task_id.as_deref() != Some(binding.task_id.as_str()) {
        return Err(denied("the run's task is not the claimed task"));
    }
    Ok(binding)
}

/// Refuse any field outside `allowed`: nothing a request adds can widen the
/// scope the broker derived.
pub(super) fn accept_fields(
    object: &Map<String, Value>,
    allowed: &[&str],
) -> Result<(), OrbitError> {
    match object.keys().find(|key| !allowed.contains(&key.as_str())) {
        Some(field) => Err(OrbitError::InvalidInput(format!(
            "a claimed worker's brokered request does not accept `{field}`"
        ))),
        None => Ok(()),
    }
}

/// Refuse a request whose `id` is not the claimed task.
pub(super) fn require_claimed_task(
    object: &Map<String, Value>,
    binding: &WorkerInvocation,
) -> Result<(), OrbitError> {
    if object.get("id").and_then(Value::as_str) != Some(binding.task_id.as_str()) {
        return Err(denied(
            "the request names a task other than the claimed task",
        ));
    }
    Ok(())
}

/// Refuse an artifact path that would leave the claimed task's artifacts.
fn require_artifact_path(path: &str) -> Result<(), OrbitError> {
    orbit_types::task::validate_relative_artifact_path(path)
        .map_err(|error| denied(&format!("the artifact path is not the task's own: {error}")))
}

/// The owner's attach input for one artifact on the claimed task.
pub(super) fn artifact_put_input(
    binding: &WorkerInvocation,
    path: &str,
    content: Vec<u8>,
    model: Option<&Value>,
) -> Value {
    let mut input = json!({
        "id": binding.task_id,
        "artifacts": [{
            "path": path,
            "media_type": media_type_for_artifact_path(path),
            "content": content,
        }],
    });
    if let Some(model) = model {
        input["model"] = model.clone();
    }
    input
}

/// Decode a put's bytes, bounded by the artifact limit.
pub(super) fn put_content(object: &Map<String, Value>) -> Result<Vec<u8>, OrbitError> {
    let encoded = object
        .get("content_base64")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let content = BASE64_STANDARD.decode(encoded).map_err(|error| {
        OrbitError::InvalidInput(format!("`content_base64` is not base64: {error}"))
    })?;
    if content.len() as u64 > MAX_TASK_ARTIFACT_CONTENT_BYTES {
        return Err(OrbitError::InvalidInput(format!(
            "the artifact exceeds the {MAX_TASK_ARTIFACT_CONTENT_BYTES} byte content limit"
        )));
    }
    Ok(content)
}

/// Execute one bridged call in the broker, under the run's own authority.
///
/// The caller has already checked the request's `cwd` and the run's tool
/// policy, and records the audit row around this.
pub(super) fn execute_brokered(
    runtime: &OrbitRuntime,
    run: &orbit_engine::PluginBrokerRun,
    tool: &str,
    input: Value,
    session: ToolSessionContext,
) -> Result<Value, OrbitError> {
    let binding = claim_binding(runtime, run)?;
    let object = input
        .as_object()
        .ok_or_else(|| OrbitError::InvalidInput("request input must be an object".into()))?;
    let path = object
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if matches!(tool, GET | PUT) && super::claimed_review::is_review_artifact(path) {
        return super::claimed_review::execute_brokered(
            runtime, run, binding, tool, object, session,
        );
    }
    let owner_input = match tool {
        TASK_SHOW => {
            accept_fields(object, &["id", "fields", "field", "run_id", "model"])?;
            require_claimed_task(object, binding)?;
            input.clone()
        }
        GET => {
            accept_fields(object, &["id", "path", "model"])?;
            require_claimed_task(object, binding)?;
            require_artifact_path(path)?;
            input.clone()
        }
        PUT => {
            accept_fields(object, &["id", "path", "content_base64", "model"])?;
            require_claimed_task(object, binding)?;
            require_artifact_path(path)?;
            artifact_put_input(binding, path, put_content(object)?, object.get("model"))
        }
        TASK_ADD => {
            if let Some(field) = object.keys().find(|key| key.starts_with('_')) {
                return Err(OrbitError::InvalidInput(format!(
                    "a claimed worker's brokered request does not accept `{field}`"
                )));
            }
            binding
                .validate_spawned_relations(&input)
                .map_err(|reason| denied(&reason))?;
            input.clone()
        }
        FRICTION_ADD => {
            if let Some(field) = object.keys().find(|key| key.starts_with('_')) {
                return Err(OrbitError::InvalidInput(format!(
                    "a claimed worker's brokered request does not accept `{field}`"
                )));
            }
            for key in ["during_task", "task_id"] {
                if object
                    .get(key)
                    .is_some_and(|value| value.as_str() != Some(binding.task_id.as_str()))
                {
                    return Err(denied(
                        "a claimed worker's friction is recorded during the claimed task only",
                    ));
                }
            }
            input.clone()
        }
        other => {
            return Err(denied(&format!(
                "'{other}' is not among the owner calls the broker carries"
            )));
        }
    };
    runtime.authorize_tool_operation(
        tool,
        &session,
        crate::runtime::tool_exec::CapabilityEnforcement::McpSessionOnly,
    )?;
    runtime.route_worker_tool(tool, owner_input, session)
}
