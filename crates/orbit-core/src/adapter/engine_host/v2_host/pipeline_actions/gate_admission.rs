//! Live admission re-check at the child-dispatch boundary [ORB-11305].

use orbit_common::observability::audit_id::audit_execution_id;
use orbit_common::protocol::tool_input::optional_string_list_alias;
use orbit_engine::DispatchError;
use orbit_store::contracts::AuditEventInsertParams;
use orbit_types::task::TaskStatus;
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::JobRunState;
use serde_json::Value;

use crate::OrbitRuntime;
use crate::runtime::task::locks::parse_task_ids;

use super::action_failed;

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
pub(super) fn gate_admission_stop(
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
pub(super) fn wait_success_status() -> String {
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
            caller_machine_name: None,
            process_machine_id: None,
            process_machine_name: None,
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
