use orbit_common::OrbitError;
use orbit_common::observability::audit_id::audit_execution_id;
use orbit_common::protocol::tool_input::optional_string_list_alias;
use orbit_engine::DispatchError;
use orbit_store::contracts::AuditEventInsertParams;
use orbit_tools::ToolContext;
use orbit_types::policy::Role;
use orbit_types::task::TaskStatus;
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::{ChildDispatchPhase, JobRunState};
use serde_json::Value;

use super::child_dispatch;
use crate::OrbitRuntime;
use crate::runtime::task::locks::parse_task_ids;

pub(super) fn validate_bundles(action: &str, input: &Value) -> Result<Value, DispatchError> {
    let bundles_raw = input
        .get("bundles")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| DispatchError::DeterministicActionFailed {
            action: action.to_string(),
            message: "`bundles` must be an array".to_string(),
        })?;
    let max_bundle_size = input
        .get("max_bundle_size")
        .and_then(Value::as_u64)
        .unwrap_or(5) as usize;
    let known: std::collections::BTreeSet<String> = input
        .get("known_task_ids")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(ToOwned::to_owned))
                .collect()
        })
        .unwrap_or_default();

    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut violations: Vec<String> = Vec::new();
    let mut bundles: Vec<Vec<String>> = Vec::with_capacity(bundles_raw.len());
    for (idx, bundle) in bundles_raw.iter().enumerate() {
        let items = bundle
            .as_array()
            .ok_or_else(|| DispatchError::DeterministicActionFailed {
                action: action.to_string(),
                message: format!("bundle[{idx}] is not an array"),
            })?;
        if items.len() > max_bundle_size {
            violations.push(format!(
                "bundle[{idx}] size {} exceeds max_bundle_size {}",
                items.len(),
                max_bundle_size
            ));
        }
        let mut bundle_ids: Vec<String> = Vec::with_capacity(items.len());
        for item in items {
            let id = item
                .as_str()
                .ok_or_else(|| DispatchError::DeterministicActionFailed {
                    action: action.to_string(),
                    message: format!("bundle[{idx}] contains a non-string task_id"),
                })?;
            if !known.is_empty() && !known.contains(id) {
                violations.push(format!("bundle[{idx}] references unknown task_id {id}"));
            }
            if !seen.insert(id.to_string()) {
                violations.push(format!("task_id {id} appears in more than one bundle"));
            }
            bundle_ids.push(id.to_string());
        }
        bundles.push(bundle_ids);
    }
    if !violations.is_empty() {
        return Err(DispatchError::DeterministicActionFailed {
            action: action.to_string(),
            message: format!("invalid bundles: {}", violations.join("; ")),
        });
    }
    Ok(serde_json::json!({
        "bundles": bundles,
        "bundle_count": bundles.len(),
    }))
}

/// Submit a child v2 Job, link it durably, then block on its terminal state.
///
/// [ORB-10971] Submission and waiting are two observable phases of one
/// activity. [ORB-11310] Child creation and the first parent link now share the
/// store transaction that checks the parent's admissions stop; the checkpoint
/// below adds independent audit evidence before the wait begins. The exact run
/// id always comes from that durable admission, never from task status or
/// timestamps.
///
/// [ORB-10819]'s blocking leaf contract is unchanged past that checkpoint: the
/// activity still returns the child's terminal wait entry, so a following
/// `pipeline_success_guard` sees exactly what it saw before.
pub(super) fn invoke_and_wait(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
    tool_context: ToolContext,
) -> Result<Value, DispatchError> {
    let parent_step_id = child_dispatch::parent_step_id(input);
    let parent_run_id = child_dispatch::parent_run_id(input);
    let invoke_context = child_invoke_context(
        tool_context.clone(),
        action,
        parent_run_id,
        parent_step_id,
        true,
    )?;
    let wait_context = tool_context;
    invoke_and_wait_with(
        runtime,
        action,
        input,
        |args| {
            runtime.run_tool_with_context_and_role(
                "orbit.pipeline.invoke",
                args,
                Role::Admin,
                invoke_context,
            )
        },
        |args| {
            runtime.run_tool_with_context_and_role(
                "orbit.pipeline.wait",
                args,
                Role::Admin,
                wait_context,
            )
        },
    )
}

/// Internal seam over the two pipeline tools so tests can drive the phase
/// ordering — checkpoint before wait, prompt failure without one — without
/// spawning real detached workers.
pub(super) fn invoke_and_wait_with<Invoke, Wait>(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
    invoke: Invoke,
    wait: Wait,
) -> Result<Value, DispatchError>
where
    Invoke: FnOnce(Value) -> Result<Value, OrbitError>,
    Wait: FnOnce(Value) -> Result<Value, OrbitError>,
{
    // [ORB-11305] Re-ask the eligibility question here, not just wherever this
    // dispatch was decided. A gate can sit in `wait_for_window` for its whole
    // budget, so the admission snapshot that queued this child may be an hour
    // stale by now.
    if let Some(stop) = gate_admission_stop(runtime, action, input)? {
        return Ok(stop);
    }

    let job_name = required_job_name(action, input)?;
    let parent_run_id = child_dispatch::parent_run_id(input);
    let parent_step_id = child_dispatch::parent_step_id(input);

    // Phase 1 — submit. A failure here never produced a durable child, so it
    // must terminalize the step now with the concrete reason attached.
    let invoke_output = invoke(invoke_args(&job_name, input)).map_err(|error| {
        let message = format!("pipeline.invoke failed: {error}");
        child_dispatch::record_dispatch_failure(
            runtime,
            action,
            parent_run_id.as_deref(),
            parent_step_id.as_deref(),
            &job_name,
            &message,
        );
        action_failed(action, message)
    })?;
    if invoke_output_skipped(&invoke_output) {
        return Ok(serde_json::json!({
            "skipped": true,
            "status": wait_success_status(),
            "reason": "admissions_stopped",
            "job_name": job_name,
        }));
    }

    // Phase 2 — link, durably, before blocking on anything.
    let dispatch = child_dispatch::dispatch_from_invoke_output(
        action,
        &job_name,
        true,
        parent_step_id.clone(),
        &invoke_output,
    )
    .inspect_err(|error| {
        child_dispatch::record_dispatch_failure(
            runtime,
            action,
            parent_run_id.as_deref(),
            parent_step_id.as_deref(),
            &job_name,
            &error.to_string(),
        );
    })?;
    child_dispatch::checkpoint_submitted_child(
        runtime,
        action,
        parent_run_id.as_deref(),
        &dispatch,
    )?;

    // Phase 3 — wait. The child is now nameable by every reader for as long
    // as this blocks.
    let child_run_id = dispatch.child_run_id.clone();
    child_dispatch::advance_child_phase(
        runtime,
        parent_run_id.as_deref(),
        &child_run_id,
        ChildDispatchPhase::Waiting,
        None,
        None,
    );
    let wait_result = wait(wait_args(&child_run_id, input));

    // Phase 4 — close the record whichever way the wait went.
    close_child_wait(
        runtime,
        action,
        parent_run_id.as_deref(),
        &child_run_id,
        wait_result,
    )
}

/// Terminalize the dispatch record from the wait's outcome and hand the child's
/// wait entry back to the caller.
///
/// A wait that errored outright is not a failed child: the parent simply stopped
/// being able to observe one it did durably submit. That is recorded as
/// `unobserved` rather than as a child status the parent never saw, so a reader
/// is never told the child failed on this evidence.
fn close_child_wait(
    runtime: &OrbitRuntime,
    action: &str,
    parent_run_id: Option<&str>,
    child_run_id: &str,
    wait_result: Result<Value, OrbitError>,
) -> Result<Value, DispatchError> {
    let wait_output = match wait_result {
        Ok(output) => output,
        Err(error) => {
            let message = format!("pipeline.wait failed: {error}");
            child_dispatch::advance_child_phase(
                runtime,
                parent_run_id,
                child_run_id,
                ChildDispatchPhase::Terminal,
                None,
                Some(message.clone()),
            );
            child_dispatch::record_child_wait_outcome(
                runtime,
                action,
                parent_run_id,
                child_run_id,
                "unobserved",
                Some(&message),
            )?;
            return Err(action_failed(action, message));
        }
    };

    let entry = wait_output
        .get("results")
        .and_then(Value::as_array)
        .and_then(|arr| arr.first())
        .cloned()
        .unwrap_or_else(|| {
            serde_json::json!({
                "run_id": child_run_id,
                "status": "pending",
            })
        });
    let status = entry
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let error_message = entry
        .get("error")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned);

    child_dispatch::advance_child_phase(
        runtime,
        parent_run_id,
        child_run_id,
        ChildDispatchPhase::Terminal,
        Some(status.clone()),
        error_message.clone(),
    );
    child_dispatch::record_child_wait_outcome(
        runtime,
        action,
        parent_run_id,
        child_run_id,
        &status,
        error_message.as_deref(),
    )?;

    Ok(entry)
}

fn required_job_name(action: &str, input: &Value) -> Result<String, DispatchError> {
    input
        .get("job_name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| action_failed(action, "missing `job_name`".to_string()))
        .map(ToOwned::to_owned)
}

fn invoke_args(job_name: &str, input: &Value) -> Value {
    let mut args = serde_json::Map::new();
    args.insert("job_name".to_string(), Value::String(job_name.to_string()));
    args.insert(
        "input".to_string(),
        input
            .get("run_input")
            .cloned()
            .unwrap_or_else(|| Value::Object(Default::default())),
    );
    if let Some(priority) = input.get("priority").cloned() {
        args.insert("priority".to_string(), priority);
    }
    Value::Object(args)
}

fn wait_args(child_run_id: &str, input: &Value) -> Value {
    let mut args = serde_json::Map::new();
    args.insert(
        "run_ids".to_string(),
        Value::Array(vec![Value::String(child_run_id.to_string())]),
    );
    if let Some(timeout) = input.get("timeout_seconds").cloned() {
        args.insert("timeout_seconds".to_string(), timeout);
    }
    if let Some(poll) = input.get("poll_interval_seconds").cloned() {
        args.insert("poll_interval_seconds".to_string(), poll);
    }
    Value::Object(args)
}

/// Submit a child v2 Job and return as soon as its Run is durable [ORB-10819].
///
/// The non-blocking counterpart to [`invoke_and_wait`], for a parent that must
/// keep working while the child runs. `workspace_auto_pipeline` dispatches
/// `epic_pipeline` this way: waiting on a multi-hour epic would consume the
/// rest of the drain window and starve the conflict-free leaves behind it.
///
/// The caller owns re-observing the child. There is deliberately no `status`
/// in the output: this action never looks at one, and reporting a freshly
/// submitted run as `pending` would invite a `pipeline_success_guard` that
/// cannot mean anything here.
pub(super) fn invoke_detached(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
    tool_context: ToolContext,
) -> Result<Value, DispatchError> {
    let job_name = required_job_name(action, input)?;
    let parent_run_id = child_dispatch::parent_run_id(input);
    let parent_step_id = child_dispatch::parent_step_id(input);

    // [ORB-11283] This read is a fast path, not the authority: a stop can land
    // after it. [ORB-11310] The pipeline submission below re-reads the same
    // state inside the SQLite transaction that creates and links the child.
    if parent_run_id
        .as_deref()
        .is_some_and(|run_id| runtime.drain_admissions_stopped(run_id))
    {
        return Ok(serde_json::json!({
            "skipped": true,
            "reason": "admissions_stopped",
            "job_name": job_name,
        }));
    }

    let invoke_context = child_invoke_context(
        tool_context,
        action,
        parent_run_id.clone(),
        parent_step_id.clone(),
        false,
    )?;
    let invoke_output = runtime
        .run_tool_with_context_and_role(
            "orbit.pipeline.invoke",
            invoke_args(&job_name, input),
            Role::Admin,
            invoke_context,
        )
        .map_err(|err| {
            let message = format!("pipeline.invoke failed: {err}");
            child_dispatch::record_dispatch_failure(
                runtime,
                action,
                parent_run_id.as_deref(),
                parent_step_id.as_deref(),
                &job_name,
                &message,
            );
            action_failed(action, message)
        })?;
    if invoke_output_skipped(&invoke_output) {
        return Ok(invoke_output);
    }

    // [ORB-10971] A detached child is linked on the same durable checkpoint as
    // a blocked-on one. The caller re-observes it later, so the linkage is the
    // only handle anyone has on it in the meantime — and it is what tells
    // cancellation to leave this child alone.
    let dispatch = child_dispatch::dispatch_from_invoke_output(
        action,
        &job_name,
        false,
        parent_step_id,
        &invoke_output,
    )?;
    child_dispatch::checkpoint_submitted_child(
        runtime,
        action,
        parent_run_id.as_deref(),
        &dispatch,
    )?;

    Ok(serde_json::json!({
        "run_id": dispatch.child_run_id,
        "job_name": dispatch.job_name,
        "queued": dispatch.queued,
        "submitted_at": invoke_output.get("submitted_at").cloned(),
    }))
}

/// Attach trusted dispatch metadata to the engine-owned parent-run context.
/// Tool input cannot select the parent whose stop governs this admission.
fn child_invoke_context(
    mut tool_context: ToolContext,
    action: &str,
    parent_run_id: Option<String>,
    parent_step_id: Option<String>,
    blocking: bool,
) -> Result<ToolContext, DispatchError> {
    if let (Some(owner), Some(parent_run_id)) = (
        tool_context.reservation_owner.as_ref(),
        parent_run_id.as_ref(),
    ) && owner.owner_run_id != *parent_run_id
    {
        return Err(action_failed(
            action,
            format!(
                "activity parent run '{}' does not match trusted run owner '{}'",
                parent_run_id, owner.owner_run_id
            ),
        ));
    }
    let Some(owner) = tool_context.reservation_owner.as_mut() else {
        return Ok(tool_context);
    };
    let mut metadata = match owner.owner_metadata_json.as_deref() {
        Some(raw) => serde_json::from_str::<Value>(raw)
            .map_err(|error| action_failed(action, format!("invalid run metadata: {error}")))?,
        None => serde_json::json!({}),
    };
    let object = metadata
        .as_object_mut()
        .ok_or_else(|| action_failed(action, "run metadata must be a JSON object".to_string()))?;
    object.insert(
        "pipeline_child_admission".to_string(),
        serde_json::json!({
            "action": action,
            "parent_step_id": parent_step_id,
            "blocking": blocking,
        }),
    );
    owner.owner_metadata_json = Some(metadata.to_string());
    Ok(tool_context)
}

/// Whether the admission path declined to create a child: an admissions stop,
/// or one of the grant-bound refusals [ORB-11332]. The reason travels with the
/// output so the coordinator's step record explains what happened.
fn invoke_output_skipped(output: &Value) -> bool {
    output.get("skipped").and_then(Value::as_bool) == Some(true)
        && output
            .get("reason")
            .and_then(Value::as_str)
            .is_some_and(|reason| !reason.is_empty())
}

/// Re-check live workflow admission for `admission_task_ids` immediately before
/// child dispatch, and return the synthetic child result when the bundle must
/// not launch.
///
/// `Ok(None)` means every task is still admissible and dispatch proceeds. Two
/// stops are distinguished because they mean opposite things to the gate:
///
/// - **stale no-op** (`review` / `done`) — the work already landed, so the
///   bundle succeeds having launched nothing.
/// - **withdrawn** ([ORB-11305]) — a human moved the task somewhere automation
///   may not start from between admission and now. The gate reports a
///   non-success child so `release_reservation` frees the reservation and
///   `require_child_success` then fails the run with the reason attached.
///
/// A task that cannot be read at all stays a hard activity failure: that is a
/// malformed bundle, not a lifecycle decision.
fn gate_admission_stop(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<Option<Value>, DispatchError> {
    let raw_task_ids = optional_string_list_alias(
        input,
        &[
            "admission_task_ids",
            "admissionTaskIds",
            "admission-task-ids",
        ],
    )
    .map_err(|err| action_failed(action, err.to_string()))?;
    let Some(raw_task_ids) = raw_task_ids else {
        return Ok(None);
    };
    let task_ids = parse_task_ids(&serde_json::json!({ "task_ids": raw_task_ids }))
        .map_err(|err| action_failed(action, err.to_string()))?;
    let workflow = input
        .get("admission_workflow")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("worktree_setup");

    let mut task_statuses = Vec::with_capacity(task_ids.len());
    let mut stale_statuses = Vec::new();
    let mut withdrawn_statuses = Vec::new();
    let mut admission_errors = Vec::new();

    for task_id in &task_ids {
        match runtime.ensure_task_can_enter_workflow_as_system(task_id, workflow) {
            Ok(task) => {
                task_statuses.push(serde_json::json!({
                    "task_id": task.id,
                    "status": task.status.to_string(),
                    "admissible": true,
                }));
            }
            Err(error) => match runtime.get_task(task_id) {
                Ok(task) => {
                    let status = task.status;
                    task_statuses.push(serde_json::json!({
                        "task_id": task.id,
                        "status": status.to_string(),
                        "admissible": false,
                    }));
                    if matches!(status, TaskStatus::Review | TaskStatus::Done) {
                        stale_statuses.push((task_id.clone(), status.to_string()));
                    } else {
                        // [ORB-11305] The task still exists, a human just moved
                        // it somewhere automation may not start from. That is a
                        // terminal answer, not a malfunction: return it as the
                        // child result so the gate's `release_reservation` step
                        // still runs before `require_child_success` fails.
                        withdrawn_statuses.push((task_id.clone(), status.to_string()));
                    }
                }
                Err(_) => admission_errors.push(error.to_string()),
            },
        }
    }

    if !admission_errors.is_empty() {
        return Err(action_failed(
            action,
            format!(
                "workflow admission check before child dispatch failed: {}",
                admission_errors.join("; ")
            ),
        ));
    }

    // Withdrawal is the stronger signal: a bundle that mixes an already-shipped
    // task with a withdrawn one must not report success.
    if !withdrawn_statuses.is_empty() {
        let reason = format!(
            "task_gate_pipeline ineligible: workflow admission for '{workflow}' refused child dispatch because {} \
             is no longer admissible (admission was granted before the status changed). \
             Return it to the backlog if this work should still run.",
            summarize_statuses(&withdrawn_statuses)
        );
        record_gate_admission_stop(
            runtime,
            action,
            input,
            &task_ids,
            &task_statuses,
            &reason,
            "withdrawn",
        )?;
        return Ok(Some(gate_admission_stop_output(
            input,
            "failed",
            "withdrawn",
            &reason,
            &task_statuses,
        )));
    }

    if stale_statuses.is_empty() {
        return Ok(None);
    }

    let reason = format!(
        "task_gate_pipeline stale/no-op: workflow admission for '{workflow}' skipped child dispatch because {}",
        summarize_statuses(&stale_statuses)
    );
    record_gate_admission_stop(
        runtime,
        action,
        input,
        &task_ids,
        &task_statuses,
        &reason,
        "stale_noop",
    )?;
    Ok(Some(gate_admission_stop_output(
        input,
        &wait_success_status(),
        "stale_noop",
        &reason,
        &task_statuses,
    )))
}

fn summarize_statuses(statuses: &[(String, String)]) -> String {
    statuses
        .iter()
        .map(|(task_id, status)| format!("{task_id}={status}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Canonical wait-envelope success token. Skip and stale-noop emitters, and
/// the admission-stop `error` branch, share this so a spelling change cannot
/// attach `error` to a successful stop.
fn wait_success_status() -> String {
    JobRunState::Success.to_string()
}

/// Build the synthetic child-run result an admission stop reports in place of a
/// real dispatch. `status` drives the gate: the canonical success token lets
/// `pipeline_success_guard` pass, anything else fails the gate *after*
/// `release_reservation` has run, and `error` is what the guard quotes.
fn gate_admission_stop_output(
    input: &Value,
    status: &str,
    outcome: &str,
    reason: &str,
    task_statuses: &[Value],
) -> Value {
    let parent_run_id = parent_run_id_or_unknown(input);
    // Synthetic: this id never resolves to a real run, it only names the stop
    // for readers. Keep the established `stale-noop-` spelling.
    let run_id_prefix = outcome.replace('_', "-");
    let mut output = serde_json::json!({
        "status": status,
        "run_id": format!("{run_id_prefix}-{parent_run_id}"),
        "skipped": true,
        "reason": reason,
        "task_statuses": task_statuses,
    });
    if status != wait_success_status() {
        output["error"] = Value::String(reason.to_string());
    }
    output
}

fn parent_run_id_or_unknown(input: &Value) -> &str {
    input
        .get("run_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("unknown")
}

/// Audit an admission stop before the gate acts on it. `outcome` is
/// `stale_noop` (already-shipped work) or `withdrawn` (a human moved the task
/// out of automation's reach [ORB-11305]); both are recorded so a run that
/// launched nothing is still explainable from the audit log alone.
fn record_gate_admission_stop(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
    task_ids: &[String],
    task_statuses: &[Value],
    reason: &str,
    outcome: &str,
) -> Result<(), DispatchError> {
    let parent_run_id = input
        .get("run_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let payload = serde_json::json!({
        "task_ids": task_ids,
        "task_statuses": task_statuses,
        "reason": reason,
        "outcome": outcome,
        "parent_run_id": parent_run_id,
    });
    let arguments_json = serde_json::to_string(&payload)
        .map_err(|err| action_failed(action, format!("serialize gate.{outcome} payload: {err}")))?;
    let execution_id = audit_execution_id("audit-gate-admission-stop");
    let working_directory = runtime.paths().repo_root.to_string_lossy().into_owned();

    runtime
        .record_audit_event(&AuditEventInsertParams {
            execution_id,
            command: format!("gate.{outcome}"),
            subcommand: None,
            tool_name: None,
            target_type: Some("task_bundle".to_string()),
            target_id: task_ids.first().cloned(),
            role: "admin".to_string(),
            status: AuditEventStatus::Success,
            exit_code: 0,
            duration_ms: 0,
            working_directory,
            arguments_json: Some(arguments_json),
            stdout_truncated: None,
            stderr_truncated: None,
            error_message: None,
            host: std::env::var("HOSTNAME").ok(),
            pid: std::process::id(),
            session_id: None,
            workspace_id: None,
            caller_machine_id: None,
            caller_host_id: None,
            process_machine_id: None,
            process_host_id: None,
            transport: None,
            effective_capabilities: Default::default(),
            origin_session_id: None,
            mcp_call_id: None,
            lease_id: None,
            task_id: task_ids.first().cloned(),
            job_run_id: parent_run_id,
            activity_id: None,
            step_index: None,
        })
        .map_err(|err| action_failed(action, format!("record gate.{outcome} audit: {err}")))
}

pub(super) fn pipeline_success_guard(action: &str, input: &Value) -> Result<Value, DispatchError> {
    let allow_non_success = input
        .get("allow_non_success")
        .map(|value| {
            value.as_bool().ok_or_else(|| {
                action_failed(action, "`allow_non_success` must be a boolean".to_string())
            })
        })
        .transpose()?
        .unwrap_or(false);
    if allow_non_success {
        return record_pipeline_results(action, input);
    }

    let context = input
        .get("context")
        .and_then(Value::as_str)
        .unwrap_or("pipeline child run");
    let mut checked_count = 0usize;
    let mut failures = Vec::new();

    if let Some(result) = input.get("result")
        && !result.is_null()
    {
        checked_count += 1;
        if let Some(failure) = pipeline_wait_entry_failure("result", result) {
            failures.push(failure);
        }
    }

    if let Some(results) = input.get("results")
        && !results.is_null()
    {
        let entries =
            results
                .as_array()
                .ok_or_else(|| DispatchError::DeterministicActionFailed {
                    action: action.to_string(),
                    message: "`results` must be an array".to_string(),
                })?;
        for (idx, entry) in entries.iter().enumerate() {
            checked_count += 1;
            if let Some(failure) = pipeline_wait_entry_failure(&format!("results[{idx}]"), entry) {
                failures.push(failure);
            }
        }
    }

    if checked_count == 0 {
        return Err(DispatchError::DeterministicActionFailed {
            action: action.to_string(),
            message: "expected `result` or `results` to check".to_string(),
        });
    }

    if !failures.is_empty() {
        return Err(DispatchError::DeterministicActionFailed {
            action: action.to_string(),
            message: format!("{context} did not succeed: {}", failures.join("; ")),
        });
    }

    Ok(serde_json::json!({
        "succeeded": true,
        "checked_count": checked_count,
    }))
}

/// Validate and retain terminal child results without converting a child
/// failure into a failure of the workspace-level sequencer.
///
/// This is deliberately an opt-in policy on the existing guard action. Gate,
/// epic, and wrapper pipelines keep their fail-fast behavior; only a caller
/// that explicitly asks to record terminal non-successes receives counts and
/// the exact entries it supplied. Structural problems remain errors because a
/// missing run id or non-terminal status is not an observed leaf outcome.
fn record_pipeline_results(action: &str, input: &Value) -> Result<Value, DispatchError> {
    let results = input
        .get("results")
        .and_then(|value| (!value.is_null()).then_some(value))
        .ok_or_else(|| action_failed(action, "expected `results` to record".to_string()))?
        .as_array()
        .ok_or_else(|| action_failed(action, "`results` must be an array".to_string()))?;
    if results.is_empty() {
        return Err(action_failed(
            action,
            "expected at least one `results` entry to record".to_string(),
        ));
    }

    let mut succeeded_count = 0usize;
    let mut non_success_count = 0usize;
    for (idx, entry) in results.iter().enumerate() {
        let label = format!("results[{idx}]");
        let run_id = entry
            .get("run_id")
            .and_then(Value::as_str)
            .filter(|run_id| !run_id.trim().is_empty())
            .ok_or_else(|| {
                action_failed(action, format!("{label} missing non-empty string run_id"))
            })?;
        let status = entry
            .get("status")
            .and_then(Value::as_str)
            .ok_or_else(|| action_failed(action, format!("{label} missing string status")))?;
        if let Some(error) = entry.get("error")
            && !error.is_null()
            && !error.is_string()
        {
            return Err(action_failed(
                action,
                format!("{label} run {run_id} has non-string error"),
            ));
        }
        match status {
            status
                if crate::application::job::pipeline::pipeline_wait_status_is_success(status) =>
            {
                succeeded_count += 1
            }
            "failed" | "cancelled" | "interrupted" | "timeout" => non_success_count += 1,
            other => {
                return Err(action_failed(
                    action,
                    format!("{label} run {run_id} has non-terminal status {other}"),
                ));
            }
        }
    }

    Ok(serde_json::json!({
        "succeeded": non_success_count == 0,
        "checked_count": results.len(),
        "succeeded_count": succeeded_count,
        "non_success_count": non_success_count,
        "results": results,
    }))
}

/// Persist each opt-in result-accounting batch independently of the loop's
/// same-id pipeline key, which is overwritten by the next iteration.
/// Parent run state still owns child linkage; this audit row owns the batch's
/// exact result list and aggregate counts for durable history readers.
pub(super) fn record_pipeline_results_audit(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
    output: &Value,
) -> Result<(), DispatchError> {
    if input.get("allow_non_success").and_then(Value::as_bool) != Some(true) {
        return Ok(());
    }

    let parent_run_id = input
        .get("run_id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let payload = serde_json::json!({
        "parent_run_id": parent_run_id,
        "parent_step_id": input.get("step_id"),
        "context": input.get("context"),
        "checked_count": output.get("checked_count"),
        "succeeded_count": output.get("succeeded_count"),
        "non_success_count": output.get("non_success_count"),
        "results": output.get("results"),
    });
    let arguments_json = serde_json::to_string(&payload).map_err(|error| {
        action_failed(
            action,
            format!("serialize pipeline.child_results payload: {error}"),
        )
    })?;

    runtime
        .record_audit_event(&AuditEventInsertParams {
            execution_id: audit_execution_id("audit-pipeline-child-results"),
            command: "pipeline.child_results".to_string(),
            subcommand: None,
            tool_name: None,
            target_type: Some("job_run".to_string()),
            target_id: parent_run_id.clone(),
            role: "admin".to_string(),
            status: AuditEventStatus::Success,
            exit_code: 0,
            duration_ms: 0,
            working_directory: runtime.paths().repo_root.to_string_lossy().into_owned(),
            arguments_json: Some(arguments_json),
            stdout_truncated: None,
            stderr_truncated: None,
            error_message: None,
            host: std::env::var("HOSTNAME").ok(),
            pid: std::process::id(),
            session_id: None,
            workspace_id: None,
            caller_machine_id: None,
            caller_host_id: None,
            process_machine_id: None,
            process_host_id: None,
            transport: None,
            effective_capabilities: Default::default(),
            origin_session_id: None,
            mcp_call_id: None,
            lease_id: None,
            task_id: None,
            job_run_id: parent_run_id,
            activity_id: None,
            step_index: None,
        })
        .map_err(|error| {
            action_failed(
                action,
                format!("record pipeline.child_results audit: {error}"),
            )
        })
}

fn pipeline_wait_entry_failure(label: &str, entry: &Value) -> Option<String> {
    let Some(status) = entry.get("status").and_then(Value::as_str) else {
        return Some(format!("{label} missing string status"));
    };
    if crate::application::job::pipeline::pipeline_wait_status_is_success(status) {
        return None;
    }

    let error = entry
        .get("error")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty());
    if entry.get("partition_decisions").is_some() {
        let applied = entry
            .get("applied_count")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let unresolved = entry
            .get("unresolved_count")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        return Some(match error {
            Some(error) => format!(
                "{label} task-pilot apply status {status}: {error} ({applied} applied, {unresolved} unresolved)"
            ),
            None => format!(
                "{label} task-pilot apply status {status} ({applied} applied, {unresolved} unresolved)"
            ),
        });
    }
    let run_id = entry
        .get("run_id")
        .and_then(Value::as_str)
        .unwrap_or("<unknown>");
    Some(match error {
        Some(error) => format!("{label} run {run_id} status {status}: {error}"),
        None => format!("{label} run {run_id} status {status}"),
    })
}

fn action_failed(action: &str, message: String) -> DispatchError {
    DispatchError::DeterministicActionFailed {
        action: action.to_string(),
        message,
    }
}

pub(super) fn gate_starvation_fail(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<Value, DispatchError> {
    let task_ids_vec: Vec<String> = input
        .get("task_ids")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(ToOwned::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let conflicts = input
        .get("conflicts")
        .cloned()
        .unwrap_or(Value::Array(Vec::new()));
    let max_wait_seconds = input.get("max_wait_seconds").and_then(Value::as_f64);
    let conflicting_files: Vec<String> = conflicts
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|entry| {
                    entry
                        .get("file")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                })
                .collect()
        })
        .unwrap_or_default();
    // The gate can starve on either axis. Reporting only `conflicting_files`
    // left a dependency-starved bundle with an empty list and no blocker
    // named at all, so carry the last-observed unmet dependency IDs too.
    let waiting_on_deps: Vec<String> = input
        .get("waiting_on_deps")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|entry| entry.as_str().map(str::trim))
                .filter(|entry| !entry.is_empty())
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default();

    let payload = serde_json::json!({
        "task_ids": task_ids_vec,
        "conflicting_files": conflicting_files,
        "conflicts": conflicts,
        "waiting_on_deps": waiting_on_deps,
        "max_wait_seconds": max_wait_seconds,
    });

    let execution_id = audit_execution_id("audit-gate-starvation");
    let working_directory = runtime.paths().repo_root.to_string_lossy().into_owned();
    runtime
        .record_audit_event(&AuditEventInsertParams {
            execution_id,
            command: "gate.starvation".to_string(),
            subcommand: None,
            tool_name: None,
            target_type: Some("task_bundle".to_string()),
            target_id: task_ids_vec.first().cloned(),
            role: "admin".to_string(),
            status: AuditEventStatus::Failure,
            exit_code: 1,
            duration_ms: 0,
            working_directory,
            arguments_json: Some(serde_json::to_string(&payload).map_err(|error| {
                DispatchError::DeterministicActionFailed {
                    action: action.to_string(),
                    message: format!("serialize gate.starvation payload: {error}"),
                }
            })?),
            stdout_truncated: None,
            stderr_truncated: None,
            error_message: Some("gate.starvation".to_string()),
            host: std::env::var("HOSTNAME").ok(),
            pid: std::process::id(),
            session_id: None,
            workspace_id: None,
            caller_machine_id: None,
            caller_host_id: None,
            process_machine_id: None,
            process_host_id: None,
            transport: None,
            effective_capabilities: Default::default(),
            origin_session_id: None,
            mcp_call_id: None,
            lease_id: None,
            task_id: task_ids_vec.first().cloned(),
            job_run_id: None,
            activity_id: None,
            step_index: None,
        })
        .map_err(|err| DispatchError::DeterministicActionFailed {
            action: action.to_string(),
            message: format!("record gate.starvation audit: {err}"),
        })?;

    Err(DispatchError::DeterministicActionFailed {
        action: action.to_string(),
        message: format!(
            "gate.starvation: admission window never opened for bundle {:?} \
             (conflicting_files={:?}, waiting_on_deps={:?}, max_wait_seconds={:?})",
            task_ids_vec, conflicting_files, waiting_on_deps, max_wait_seconds
        ),
    })
}
