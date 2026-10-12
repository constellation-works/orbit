//! Child Job submission: blocking `invoke_and_wait` and non-blocking `invoke_detached`.

use orbit_common::OrbitError;
use orbit_engine::DispatchError;
use orbit_tools::ToolContext;
use orbit_types::policy::Role;
use orbit_types::workflow::{CHILD_TIMEOUT_ERROR_CODE, ChildDispatchPhase};
use serde_json::Value;

use super::super::child_dispatch;
use crate::OrbitRuntime;
use crate::application::job::{kept_candidate, kept_clause};

use super::gate_admission::{gate_admission_stop, wait_success_status};

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
/// activity still returns the child's terminal status as data, so a following
/// `pipeline_success_guard` sees exactly what it saw before. It returns a
/// bounded summary of the wait entry, never the child's pipeline: the child's
/// own run keeps that, and a reader fetches it by `run_id` [ORB-14587].
///
/// [ORB-15643] A caller passing `on_timeout: cancel_child` never outlives its
/// child: a wait that expires on an unfinished child cancels it, keeping its
/// candidate, and reports `timeout` with a `child_timeout` error.
pub(in super::super) fn invoke_and_wait(
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
        DispatchError::action_failed(action, message)
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
        input,
        parent_run_id.as_deref(),
        &child_run_id,
        wait_result,
    )
}

/// Terminalize the dispatch record from the wait's outcome and hand the
/// summary of the child's wait entry back to the caller.
///
/// A wait that errored outright is not a failed child: the parent simply stopped
/// being able to observe one it did durably submit. That is recorded as
/// `unobserved` rather than as a child status the parent never saw, so a reader
/// is never told the child failed on this evidence.
fn close_child_wait(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
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
            return Err(DispatchError::action_failed(action, message));
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
    let unfinished = entry
        .get("status")
        .and_then(Value::as_str)
        .is_some_and(is_unfinished);
    let (entry, cancelled) = if unfinished && cancels_child_on_timeout(input) {
        stop_unfinished_child(runtime, input, parent_run_id, child_run_id, entry)
    } else {
        (entry, None)
    };
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

    let mut summary = child_result_summary(&entry);
    // A child left running, or one whose cancel failed, is named live so the
    // caller does not take it for settled.
    if is_unfinished(&status) && child_is_live(runtime, child_run_id) {
        summary["child_live"] = Value::Bool(true);
    }
    if let Some(cancelled) = cancelled {
        summary["child_cancelled"] = Value::Bool(true);
        if let Some(candidate) = cancelled.candidate {
            summary["candidate"] = candidate;
        }
    }
    Ok(summary)
}

/// A wait status that may name a child still pending or running: the wait's
/// own deadline projects one as `timeout`.
fn is_unfinished(status: &str) -> bool {
    matches!(status, "timeout" | "pending" | "running")
}

/// Whether the caller asked that a child its wait leaves unfinished be
/// cancelled rather than left running [ORB-15643]. Other callers keep the
/// original contract: a `timeout` that may name a child still running.
fn cancels_child_on_timeout(input: &Value) -> bool {
    input.get("on_timeout").and_then(Value::as_str) == Some("cancel_child")
}

/// Whether `child_run_id` is still pending or running. An unreadable child
/// counts as live: a caller is never told an unobserved child settled.
fn child_is_live(runtime: &OrbitRuntime, child_run_id: &str) -> bool {
    match runtime.get_job_run_backend(child_run_id) {
        Ok(Some(run)) => !run.state.is_terminal(),
        Ok(None) => false,
        Err(error) => {
            tracing::warn!(child_run_id, %error, "could not read the waited-on child");
            true
        }
    }
}

/// A child the wait's expiry cancelled, with the candidate it kept.
struct CancelledChild {
    candidate: Option<Value>,
}

/// Settle a child the wait did not see finish, so the parent never ends with
/// it still running [ORB-15643].
///
/// A live child is cancelled: its candidate is kept by its own failure
/// handoff and its task goes back to the backlog naming the timeout. The
/// entry stays `timeout`, with an error that leads with `child_timeout` and
/// names what was kept. A child that finished after the wait's last look, or
/// while it was being cancelled, reports its real outcome instead.
fn stop_unfinished_child(
    runtime: &OrbitRuntime,
    input: &Value,
    parent_run_id: Option<&str>,
    child_run_id: &str,
    entry: Value,
) -> (Value, Option<CancelledChild>) {
    if !child_is_live(runtime, child_run_id) {
        // The entry's `timeout` may be the wait's projection onto a child
        // that settled after its last look; report what it settled as.
        let settled = observe_settled_child(runtime, child_run_id).unwrap_or(entry);
        return (settled, None);
    }
    let waited = match input.get("timeout_seconds") {
        Some(Value::Number(seconds)) => format!("{seconds} s"),
        Some(Value::String(seconds)) => format!("{seconds} s"),
        _ => "its wait deadline".to_string(),
    };
    let reason = format!(
        "{CHILD_TIMEOUT_ERROR_CODE}: parent run {} stopped waiting for child run {child_run_id} \
         after {waited}",
        parent_run_id.unwrap_or("-"),
    );
    let mut stopped = entry;
    match runtime.cancel_timed_out_child(child_run_id, &reason) {
        Ok(result) if result.outcome == "cancelled" => {
            let handoff = runtime
                .read_run_state(child_run_id)
                .ok()
                .flatten()
                .and_then(|state| state.failure_activity_checkpoint)
                .map(|checkpoint| checkpoint.output);
            let kept = handoff
                .as_ref()
                .map_or_else(|| "it kept no candidate".to_string(), kept_clause);
            stopped["status"] = Value::String("timeout".to_string());
            stopped["error"] =
                Value::String(format!("{reason}; the child was cancelled and {kept}"));
            let candidate = handoff.as_ref().and_then(kept_candidate);
            (stopped, Some(CancelledChild { candidate }))
        }
        // The child reached its own terminal state first.
        Ok(_) => (
            observe_settled_child(runtime, child_run_id).unwrap_or(stopped),
            None,
        ),
        Err(error) => {
            stopped["status"] = Value::String("timeout".to_string());
            stopped["error"] =
                Value::String(format!("{reason}; cancelling the child failed: {error}"));
            (stopped, None)
        }
    }
}

/// The settled child's own wait entry, read without waiting.
fn observe_settled_child(runtime: &OrbitRuntime, child_run_id: &str) -> Option<Value> {
    runtime
        .wait_pipeline_runs(&[child_run_id.to_string()], 0, 1, None)
        .ok()
        .and_then(|result| result.results.into_iter().next())
        .and_then(|entry| serde_json::to_value(entry).ok())
}

/// The wait entry fields a parent step, guard or reader uses. The child's
/// pipeline stays out: the parent would otherwise store a copy of it in its
/// own state, once more for every fan-in alias of the step [ORB-14587].
fn child_result_summary(entry: &Value) -> Value {
    let summary = ["run_id", "status", "finished_at", "duration_ms", "error"]
        .into_iter()
        .filter_map(|field| {
            entry
                .get(field)
                .filter(|value| !value.is_null())
                .map(|value| (field.to_string(), value.clone()))
        })
        .collect();
    Value::Object(summary)
}

fn required_job_name(action: &str, input: &Value) -> Result<String, DispatchError> {
    input
        .get("job_name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| DispatchError::action_failed(action, "missing `job_name`".to_string()))
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
/// keep working while the child runs. `workspace_auto_pipeline` dispatches its
/// leaves this way: waiting on a multi-hour child would consume the rest of the
/// drain window and starve the conflict-free work behind it.
///
/// The caller owns re-observing the child. There is deliberately no `status`
/// in the output: this action never looks at one, and reporting a freshly
/// submitted run as `pending` would invite a `pipeline_success_guard` that
/// cannot mean anything here.
pub(in super::super) fn invoke_detached(
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
            DispatchError::action_failed(action, message)
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
        return Err(DispatchError::action_failed(
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
        Some(raw) => serde_json::from_str::<Value>(raw).map_err(|error| {
            DispatchError::action_failed(action, format!("invalid run metadata: {error}"))
        })?,
        None => serde_json::json!({}),
    };
    let object = metadata.as_object_mut().ok_or_else(|| {
        DispatchError::action_failed(action, "run metadata must be a JSON object".to_string())
    })?;
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
