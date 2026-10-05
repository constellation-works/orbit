//! The claimed before-PR reviewer's artifact route through its run's broker.
//!
//! A claimed leaf's reviewer reads its pinned manifest and attaches its report
//! on the owner's task. The owner is another machine, reached over SSH, and
//! the agent sandbox masks `~/.ssh`, so the nested `orbit` cannot open that
//! route itself. With `ORBIT_PLUGIN_BROKER` set it hands exactly these two
//! calls to the broker in the unconfined step runner, which already holds the
//! claim's owner route. Nothing else is forwarded: every other coordination
//! tool keeps its existing route, and the broker answers no other built-in.
//!
//! The broker decides everything from its own records. The worker binding
//! names the task, claim, owner and leaf run; the run's dispatch record names
//! the activity; the review ledger names the one attempt whose reviewer is
//! running in this run. The request contributes only the artifact bytes the
//! reviewer wrote, which the broker validates before the owner's claim
//! transaction persists them. The broker never opens a path the agent named:
//! the nested `orbit` reads the source inside the sandbox, no-follow, under
//! the workspace confinement `orbit.task.artifact.put` already applies.

use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::task::{MAX_TASK_ARTIFACT_CONTENT_BYTES, media_type_for_artifact_path};
use orbit_types::tool::{ToolSessionContext, WorkerInvocation};
use orbit_types::workflow::{
    REVIEW_CONTRACT_VERSION, REVIEW_MANIFEST_ARTIFACT, REVIEW_REPORT_ARTIFACT, ReviewAttemptState,
    ReviewManifest, ReviewReport,
};
use serde_json::{Map, Value, json};

use super::execute::ToolEntryPoint;
use crate::OrbitRuntime;

pub(super) use crate::runtime::plugin::broker::is_claimed_review_artifact;

const GET: &str = "orbit.task.artifact.get";
const PUT: &str = "orbit.task.artifact.put";

/// Whether this process is a claimed worker whose owner is another machine:
/// the only caller whose artifact calls need the bridge. A worker on its
/// owner's machine reaches the owner store in process and keeps that route.
fn remote_owner<'a>(
    binding: Option<&'a WorkerInvocation>,
    process_machine_id: Option<&str>,
) -> Option<&'a WorkerInvocation> {
    let binding = binding?;
    let local = process_machine_id?;
    (binding.execution.machine_id == local && binding.owner_machine_id != local).then_some(binding)
}

/// How a claimed reviewer's artifact call leaves the nested `orbit`.
#[derive(Debug)]
pub enum ClaimedReviewRoute {
    /// The broker answered, or could not be reached. The broker records the
    /// call's audit row when it answered; the caller audits a missing or
    /// unusable response.
    Forwarded(Result<Value, OrbitError>),
    /// Refused before a valid broker response: no broker inside a masked
    /// sandbox, a request the nested process could not prepare, or an
    /// unavailable/invalid transport response. Nothing else can be relied on
    /// to have recorded it, so the caller audits the refusal.
    Refused(OrbitError),
}

/// Route a claimed reviewer's artifact call from the nested `orbit` (CLI or
/// MCP) to its run's broker.
///
/// `None` leaves the call on the caller's existing route: another tool, no
/// remote owner, or an unsandboxed worker without a broker, which can reach
/// the owner itself. Inside a masked agent sandbox without a broker the call
/// is refused with the cause, rather than attempting an SSH route that can
/// only fail on the masked `~/.ssh`.
#[allow(clippy::too_many_arguments)]
pub fn bridge_claimed_review_artifact(
    global_root: &Path,
    binding: Option<&WorkerInvocation>,
    process_machine_id: Option<&str>,
    name: &str,
    input: &Value,
    cwd: &Path,
    workspace_root: &Path,
    entry_point: ToolEntryPoint,
) -> Option<ClaimedReviewRoute> {
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

/// [`bridge_claimed_review_artifact`] with the broker socket, if any, given.
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
) -> Option<ClaimedReviewRoute> {
    if !is_claimed_review_artifact(name) {
        return None;
    }
    let binding = remote_owner(binding, process_machine_id)?;
    let Some(socket) = socket else {
        return refuse_unbridged(global_root, Some(binding), process_machine_id, name)
            .err()
            .map(ClaimedReviewRoute::Refused);
    };
    let request = match bridged_input(binding, name, input, cwd, workspace_root) {
        Ok(request) => request,
        Err(error) => return Some(ClaimedReviewRoute::Refused(error)),
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
        Ok(value) => ClaimedReviewRoute::Forwarded(Ok(value)),
        Err(crate::runtime::plugin::broker::ForwardCallError::BrokerAudit(error)) => {
            ClaimedReviewRoute::Forwarded(Err(error))
        }
        Err(crate::runtime::plugin::broker::ForwardCallError::CallerAudit(error)) => {
            ClaimedReviewRoute::Refused(coordinator_unavailable(name, error))
        }
    })
}

/// Name what an unreachable broker means for the reviewer: its run's step
/// runner is gone, so the call cannot be retried from inside the sandbox.
fn coordinator_unavailable(name: &str, error: OrbitError) -> OrbitError {
    match error {
        OrbitError::RemoteTool {
            code,
            message,
            mut payload,
        } if code == "plugin_broker_unavailable" => {
            let message = format!(
                "'{name}' could not reach this run's coordinator ({message}); the step runner \
                 that carries a claimed reviewer's artifact calls has stopped or is restarting. \
                 Do not read SSH credentials or route around the sandbox; report the review \
                 incomplete and let the run be retried"
            );
            payload["message"] = Value::String(message.clone());
            OrbitError::RemoteTool {
                code,
                message,
                payload,
            }
        }
        other => other,
    }
}

/// Refuse a claimed reviewer's artifact call that has no broker to go to from
/// inside a masked agent sandbox: the SSH route to the owner could only fail
/// on the masked `~/.ssh`, so the refusal names the cause instead.
fn refuse_unbridged(
    global_root: &Path,
    binding: Option<&WorkerInvocation>,
    process_machine_id: Option<&str>,
    name: &str,
) -> Result<(), OrbitError> {
    if is_claimed_review_artifact(name)
        && remote_owner(binding, process_machine_id).is_some()
        && crate::runtime::plugin::sandbox_mask::plugin_trees_masked(global_root)
    {
        return Err(OrbitError::CapabilityDenied(format!(
            "'{name}' for a claimed task reaches the owner only through this run's coordinator: \
             the agent sandbox masks ~/.ssh, and ORBIT_PLUGIN_BROKER is not set. Do not read SSH \
             credentials or loosen the sandbox; report the review incomplete so the run can be \
             retried on a current binary"
        )));
    }
    Ok(())
}

/// The request the broker receives: the task, the artifact path and, for a
/// put, the bytes read here, base64-encoded so a full-size artifact fits the
/// broker's frame. A `workspace` may only confirm the binding; it is dropped,
/// since the broker routes to the claim's owner regardless, as is the MCP
/// envelope's `_meta`, which describes this process rather than the call.
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

/// The claim and review attempt a broker serves, derived from host records.
struct ClaimedReviewScope<'a> {
    binding: &'a WorkerInvocation,
    attempt_id: String,
    lineage_key: String,
}

impl<'a> ClaimedReviewScope<'a> {
    /// Refuse unless this run is a claimed leaf's reviewer with exactly one
    /// admitted attempt whose reviewer is running in it now.
    fn derive(
        runtime: &'a OrbitRuntime,
        run: &orbit_engine::PluginBrokerRun,
    ) -> Result<Self, OrbitError> {
        let denied = |reason: &str| {
            OrbitError::PolicyDenied(format!("claimed_review_bridge_refused: {reason}"))
        };
        if run.activity_name != orbit_engine::review_gate::REVIEWER_ACTIVITY {
            return Err(denied(&format!(
                "the broker carries review artifacts only for the before-PR reviewer, not \
                 activity '{}'",
                run.activity_name
            )));
        }
        let binding = runtime
            .worker_invocation()
            .ok_or_else(|| denied("this run is not bound to a claim"))?;
        if remote_owner(Some(binding), runtime.automation_machine_identity()).is_none() {
            return Err(denied(
                "the claim's owner is this machine, which the reviewer reaches directly",
            ));
        }
        let job_run_id = run
            .job_run_id
            .as_deref()
            .ok_or_else(|| denied("the run has no job-run authority"))?;
        if run.task_id.as_deref() != Some(binding.task_id.as_str()) {
            return Err(denied("the run's task is not the claimed task"));
        }
        let now = Utc::now();
        let mut held = Vec::new();
        for ledger in runtime
            .review_store()?
            .review_ledgers_held_by(&runtime.workspace_id()?, job_run_id)?
        {
            if !ledger.task_ids.contains(&binding.task_id) {
                continue;
            }
            for attempt in &ledger.attempts {
                let running = attempt
                    .reviewer_running
                    .as_ref()
                    .is_some_and(|running| running.run_id == job_run_id && now < running.deadline);
                if running
                    && attempt.state == ReviewAttemptState::Open
                    && attempt.run_id == binding.bound_run_id
                {
                    held.push((attempt.attempt_id.clone(), ledger.lineage_key.clone()));
                }
            }
        }
        match held.as_slice() {
            [(attempt_id, lineage_key)] => Ok(Self {
                binding,
                attempt_id: attempt_id.clone(),
                lineage_key: lineage_key.clone(),
            }),
            [] => Err(denied(
                "review_attempt_stale: no admitted review attempt has its reviewer running in \
                 this run; the attempt was settled, released or timed out",
            )),
            _ => Err(denied(
                "more than one open review attempt names this run; refusing to choose",
            )),
        }
    }

    fn task_id(&self) -> &str {
        &self.binding.task_id
    }
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
    let scope = ClaimedReviewScope::derive(runtime, run)?;
    let object = input
        .as_object()
        .ok_or_else(|| OrbitError::InvalidInput("request input must be an object".into()))?;
    let allowed: &[&str] = if tool == PUT {
        &["id", "path", "content_base64", "model"]
    } else {
        &["id", "path", "model"]
    };
    if let Some(field) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        return Err(OrbitError::InvalidInput(format!(
            "claimed review artifact request does not accept `{field}`"
        )));
    }
    let field = |key: &str| object.get(key).and_then(Value::as_str).unwrap_or_default();
    if field("id") != scope.task_id() {
        return Err(OrbitError::PolicyDenied(
            "claimed_review_bridge_refused: the request names a task other than the claimed task"
                .into(),
        ));
    }
    let expected = if tool == PUT {
        REVIEW_REPORT_ARTIFACT
    } else {
        REVIEW_MANIFEST_ARTIFACT
    };
    if field("path") != expected {
        return Err(OrbitError::PolicyDenied(format!(
            "claimed_review_bridge_refused: '{tool}' carries only `{expected}` for the claimed \
             reviewer"
        )));
    }
    runtime.authorize_tool_operation(
        tool,
        &session,
        crate::runtime::tool_exec::CapabilityEnforcement::McpSessionOnly,
    )?;
    let mut owner_input = Map::new();
    owner_input.insert("id".into(), json!(scope.task_id()));
    if let Some(model) = object.get("model") {
        owner_input.insert("model".into(), model.clone());
    }
    if tool == GET {
        owner_input.insert("path".into(), json!(expected));
        let output = runtime.route_worker_tool(GET, Value::Object(owner_input), session)?;
        scope.check_manifest(&output)?;
        return Ok(output);
    }
    let content = BASE64_STANDARD
        .decode(field("content_base64"))
        .map_err(|error| {
            OrbitError::InvalidInput(format!("`content_base64` is not base64: {error}"))
        })?;
    scope.check_report(&content)?;
    owner_input.insert(
        "artifacts".into(),
        json!([{
            "path": expected,
            "media_type": media_type_for_artifact_path(expected),
            "content": content,
        }]),
    );
    runtime.route_worker_tool(PUT, Value::Object(owner_input), session)
}

impl ClaimedReviewScope<'_> {
    /// The manifest the owner returned must be the admitted attempt's: the
    /// gate writes it once per attempt, so another attempt's manifest is stale.
    fn check_manifest(&self, output: &Value) -> Result<(), OrbitError> {
        let bytes = match (output.get("content"), output.get("content_base64")) {
            (Some(Value::String(text)), _) => text.as_bytes().to_vec(),
            (_, Some(Value::String(encoded))) => {
                BASE64_STANDARD.decode(encoded).map_err(|error| {
                    OrbitError::Execution(format!("owner manifest payload: {error}"))
                })?
            }
            _ => {
                return Err(OrbitError::Execution(
                    "the owner answered the manifest read without its bytes".into(),
                ));
            }
        };
        let manifest: ReviewManifest = serde_json::from_slice(&bytes).map_err(|error| {
            OrbitError::Execution(format!("{REVIEW_MANIFEST_ARTIFACT} is unreadable: {error}"))
        })?;
        if manifest.attempt_id != self.attempt_id
            || manifest.lineage_key != self.lineage_key
            || !manifest.task_ids.iter().any(|id| id == self.task_id())
        {
            return Err(OrbitError::PolicyDenied(format!(
                "claimed_review_bridge_refused: review_manifest_stale: the owner's manifest is \
                 for attempt '{}', not the running attempt '{}'",
                manifest.attempt_id, self.attempt_id
            )));
        }
        Ok(())
    }

    /// A report must be the review contract's, for the running attempt. The
    /// owner's attach validates the contract again; checking here keeps a
    /// report for another attempt from reaching the owner at all.
    fn check_report(&self, content: &[u8]) -> Result<(), OrbitError> {
        if content.len() as u64 > MAX_TASK_ARTIFACT_CONTENT_BYTES {
            return Err(OrbitError::InvalidInput(format!(
                "{REVIEW_REPORT_ARTIFACT} exceeds the {MAX_TASK_ARTIFACT_CONTENT_BYTES} byte \
                 content limit"
            )));
        }
        let report = ReviewReport::parse(content).map_err(|error| {
            OrbitError::InvalidInput(format!(
                "{REVIEW_REPORT_ARTIFACT} does not match the review report contract: {error}"
            ))
        })?;
        if report.schema_version != REVIEW_CONTRACT_VERSION {
            return Err(OrbitError::InvalidInput(format!(
                "{REVIEW_REPORT_ARTIFACT} has schema_version {}; the review report contract is \
                 version {REVIEW_CONTRACT_VERSION}",
                report.schema_version
            )));
        }
        if report.attempt_id != self.attempt_id {
            return Err(OrbitError::PolicyDenied(format!(
                "claimed_review_bridge_refused: the report names attempt '{}', not the running \
                 attempt '{}'",
                report.attempt_id, self.attempt_id
            )));
        }
        Ok(())
    }
}
