//! A claimed leaf's own run evidence through its run's broker [ORB-14661].
//!
//! A claimed leaf's final recovery diagnoses its run from the review and
//! delivery evidence its procedure names, and the leaf's sandbox reaches
//! neither: the before-PR gate's `review-*` artifacts live on the owner's task
//! behind the reviewer-only scope ([`super::claimed_review`]), and the owner
//! holds no record of the follower's leaf run, so its delivery view of the
//! leaf can only be "job run not found".
//!
//! The broker answers both from host records alone, read-only:
//!
//! - a `review-*` read from the `final_recovery` activity of the claimed leaf,
//!   for exactly the gate's named artifacts ([`RECOVERY_REVIEW_READS`]), named
//!   in their canonical spelling, while the leaf still holds its claim and its
//!   final recovery is admitted and undecided; no `review-*` write is carried
//!   for it;
//! - a delivery view of the claimed task for the leaf run itself, from this
//!   executor's own record of the leaf, for any claimed worker of that leaf;
//!   another run's delivery is refused, since a claim delivers through its
//!   leaf only.

use orbit_common::OrbitError;
use orbit_types::task::TASK_SHOW_DELIVERY_FIELD;
use orbit_types::tool::{ToolSessionContext, WorkerInvocation};
use orbit_types::workflow::{
    FINAL_RECOVERY_ACTIVITY, REVIEW_BASELINE_ARTIFACT, REVIEW_GATE_ARTIFACT,
    REVIEW_MANIFEST_ARTIFACT, REVIEW_REPORT_ARTIFACT, REVIEW_REPORT_HISTORY_ARTIFACT,
};
use serde_json::{Map, Value, json};

use super::claimed_owner::{GET, PUT, TASK_SHOW, accept_fields, require_claimed_task};
use crate::OrbitRuntime;
use crate::application::job::claimed::ClaimedLeaf;
use crate::runtime::tool_exec::CapabilityEnforcement;

/// The before-PR gate's artifacts a claimed leaf's final recovery reads: the
/// pinned manifest, the reviewer's report and its history, the settled gate
/// result and the host's baseline runs. Any other `review-*` path stays the
/// reviewer's.
pub(super) const RECOVERY_REVIEW_READS: [&str; 5] = [
    REVIEW_MANIFEST_ARTIFACT,
    REVIEW_REPORT_ARTIFACT,
    REVIEW_REPORT_HISTORY_ARTIFACT,
    REVIEW_GATE_ARTIFACT,
    REVIEW_BASELINE_ARTIFACT,
];

/// Whether this run is a final recovery, whose `review-*` calls this scope
/// decides instead of the reviewer's.
pub(super) fn is_final_recovery(run: &orbit_engine::PluginBrokerRun) -> bool {
    run.activity_name == FINAL_RECOVERY_ACTIVITY
}

/// The claimed leaf whose final recovery this run is, while that recovery
/// is live, or the host's reason why not.
fn recovering_leaf(
    runtime: &OrbitRuntime,
    run: &orbit_engine::PluginBrokerRun,
) -> Result<ClaimedLeaf, String> {
    let job_run_id = run
        .job_run_id
        .as_deref()
        .ok_or_else(|| "the run has no job-run authority".to_string())?;
    runtime
        .claimed_leaf_in_final_recovery(job_run_id)
        .map_err(reason)
}

/// The claimed leaf this run belongs to, while it holds its claim.
fn live_leaf(
    runtime: &OrbitRuntime,
    run: &orbit_engine::PluginBrokerRun,
) -> Result<ClaimedLeaf, String> {
    let job_run_id = run
        .job_run_id
        .as_deref()
        .ok_or_else(|| "the run has no job-run authority".to_string())?;
    runtime.live_claimed_leaf(job_run_id).map_err(reason)
}

fn reason(error: OrbitError) -> String {
    match error {
        OrbitError::PolicyDenied(reason) => reason,
        other => other.to_string(),
    }
}

/// Execute one `review-*` artifact call of a claimed leaf's final recovery,
/// for the claim [`super::claimed_owner`] derived. `path` is the request's
/// canonical artifact path; the request must also have spelled it that way,
/// so a name that only normalises to a gate artifact is refused rather than
/// read.
pub(super) fn execute_review_read(
    runtime: &OrbitRuntime,
    run: &orbit_engine::PluginBrokerRun,
    binding: &WorkerInvocation,
    tool: &str,
    object: &Map<String, Value>,
    path: &str,
    session: ToolSessionContext,
) -> Result<Value, OrbitError> {
    if tool == PUT {
        return Err(super::claimed_review::denied(&format!(
            "review_write_refused: final recovery reads its run's review evidence; '{PUT}' of a \
             `review-*` artifact stays the before-PR reviewer's"
        )));
    }
    let leaf = recovering_leaf(runtime, run).map_err(|why| super::claimed_review::denied(&why))?;
    accept_fields(object, &["id", "path", "model"])?;
    require_claimed_task(object, binding)?;
    let spelled = object.get("path").and_then(Value::as_str);
    if tool != GET || spelled != Some(path) || !RECOVERY_REVIEW_READS.contains(&path) {
        return Err(super::claimed_review::denied(&format!(
            "review_read_refused: final recovery reads only `{}`, each named exactly",
            RECOVERY_REVIEW_READS.join("`, `")
        )));
    }
    runtime.authorize_tool_operation(GET, &session, CapabilityEnforcement::McpSessionOnly)?;
    let mut input = json!({"id": leaf.claim.task_id, "path": path});
    if let Some(model) = object.get("model") {
        input["model"] = model.clone();
    }
    runtime.route_worker_tool(GET, input, session)
}

/// Whether an `orbit.task.show` request asks for the delivery view, in any
/// of the spellings the tool accepts for `field`/`fields`.
pub(super) fn wants_delivery(object: &Map<String, Value>) -> bool {
    requested_fields(object)
        .iter()
        .any(|field| field == TASK_SHOW_DELIVERY_FIELD)
}

fn requested_fields(object: &Map<String, Value>) -> Vec<String> {
    ["fields", "field"]
        .iter()
        .filter_map(|key| object.get(*key))
        .flat_map(|value| match value {
            Value::String(text) => text.split(',').map(str::to_string).collect(),
            Value::Array(items) => items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
            _ => Vec::new(),
        })
        .map(|field| field.trim().to_string())
        .filter(|field| !field.is_empty())
        .collect()
}

/// Answer a claimed worker's delivery view of its claimed task from this
/// executor's record of the leaf run. The owner has no record of the leaf,
/// so the view is never forwarded. Only the leaf run is readable, named or
/// defaulted; a final recovery reads it only while that recovery is live.
pub(super) fn delivery(
    runtime: &OrbitRuntime,
    run: &orbit_engine::PluginBrokerRun,
    binding: &WorkerInvocation,
    object: &Map<String, Value>,
    session: ToolSessionContext,
) -> Result<Value, OrbitError> {
    if requested_fields(object) != [TASK_SHOW_DELIVERY_FIELD] {
        return Err(OrbitError::InvalidInput(format!(
            "`{TASK_SHOW_DELIVERY_FIELD}` cannot be combined with other fields"
        )));
    }
    match object.get("run_id") {
        None => {}
        Some(Value::String(run_id)) if run_id.trim() == binding.bound_run_id => {}
        Some(_) => {
            return Err(super::claimed_owner::denied(
                "delivery_run_refused: a claimed worker reads the delivery of its own leaf run \
                 only; omit `run_id` or name that run",
            ));
        }
    }
    let leaf = if is_final_recovery(run) {
        recovering_leaf(runtime, run)
    } else {
        live_leaf(runtime, run)
    }
    .map_err(|why| super::claimed_owner::denied(&why))?;
    runtime.authorize_tool_operation(TASK_SHOW, &session, CapabilityEnforcement::McpSessionOnly)?;
    let observation = runtime.observe_claimed_leaf_delivery(&leaf)?;
    serde_json::to_value(observation)
        .map_err(|error| OrbitError::Execution(format!("serialize task delivery: {error}")))
}
