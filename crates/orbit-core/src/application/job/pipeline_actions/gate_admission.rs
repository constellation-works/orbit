//! Live admission re-check at the child-dispatch boundary [ORB-11305].

use std::collections::BTreeSet;

use orbit_common::protocol::tool_input::optional_string_list_alias;
use orbit_engine::DispatchError;
use orbit_types::task::TaskStatus;
use orbit_types::telemetry::AuditEventStatus;
use orbit_types::workflow::JobRunState;
use serde_json::Value;

use crate::OrbitRuntime;
use crate::application::job::PipelineWaitStatus;
use crate::runtime::audit::system::{
    AuditRunAttribution, SystemAuditEvent, record_system_audit_event,
};
use crate::runtime::task::locks::parse_task_ids;

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
/// - **admission refused** — the task is still `backlog` / `in-progress` but
///   admission refuses it (pilot hold, native-OS wait, unreadable pilot
///   receipt). Gate behavior matches withdrawn; the reason quotes the
///   admission error instead of claiming a status change.
/// - **claimed elsewhere** ([ORB-13918]) — a distributed pull claimed the task
///   after the drain admitted it. The claim is the one execution; the bundle
///   succeeds having launched nothing, exactly like a stale no-op, so the
///   local gate neither runs it a second time nor fails over work in flight.
///
/// - **starvation deferred** ([ORB-15514]) — the gate's wait budget expired
///   without a reservation. The task stays `backlog` for a later pass; the
///   bundle succeeds having launched nothing, and the stop carries the
///   deferral naming what blocked it.
///
/// A task that cannot be read at all stays a hard activity failure: that is a
/// malformed bundle, not a lifecycle decision.
pub(crate) fn gate_admission_stop(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<Option<Value>, DispatchError> {
    if let Some(deferral) = starvation_deferral(input) {
        // `gate_starvation_defer` already audited it as `gate.starvation`.
        let reason = format!(
            "task_gate_pipeline deferred: {}",
            deferral
                .get("detail")
                .and_then(Value::as_str)
                .unwrap_or("admission window never opened")
        );
        let mut output = gate_admission_stop_output(
            input,
            &wait_success_status(),
            "starvation_deferred",
            &reason,
            &[],
        );
        output["deferral"] = deferral.clone();
        return Ok(Some(output));
    }
    let raw_task_ids = optional_string_list_alias(
        input,
        &[
            "admission_task_ids",
            "admissionTaskIds",
            "admission-task-ids",
        ],
    )
    .map_err(|err| DispatchError::action_failed(action, err.to_string()))?;
    let Some(raw_task_ids) = raw_task_ids else {
        return Ok(None);
    };
    let task_ids = parse_task_ids(&serde_json::json!({ "task_ids": raw_task_ids }))
        .map_err(|err| DispatchError::action_failed(action, err.to_string()))?;
    let workflow = input
        .get("admission_workflow")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("worktree_setup");

    // A live claim leaves its task `in-progress`, which workflow admission
    // still accepts, so status alone cannot stop a gate that was queued before
    // the claim landed.
    let claimed: BTreeSet<String> = runtime
        .inspect_execution_claims()
        .map_err(|err| {
            DispatchError::action_failed(action, format!("read execution claims: {err}"))
        })?
        .into_iter()
        .map(|inspection| inspection.claim)
        .filter(|claim| claim.phase.protects_footprint())
        .map(|claim| claim.task_id)
        .collect();

    let mut task_statuses = Vec::with_capacity(task_ids.len());
    let mut stale_statuses = Vec::new();
    let mut withdrawn_statuses = Vec::new();
    let mut claimed_statuses = Vec::new();
    let mut refused = Vec::new();
    let mut admission_errors = Vec::new();

    for task_id in &task_ids {
        if claimed.contains(task_id) {
            let task = runtime.get_task(task_id).map_err(|err| {
                DispatchError::action_failed(action, format!("load task {task_id}: {err}"))
            })?;
            task_statuses.push(serde_json::json!({
                "task_id": task.id,
                "status": task.status.to_string(),
                "admissible": false,
            }));
            claimed_statuses.push((task.id, "claimed".to_string()));
            continue;
        }
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
                    } else if OrbitRuntime::workflow_admissible_statuses().contains(&status) {
                        // The status never left the admissible set, so the
                        // refusal is the admission check's own: a pilot hold,
                        // a native-OS wait or an unreadable pilot receipt.
                        // Keep its text; "the status changed" would be false.
                        refused.push((task_id.clone(), status.to_string(), error.to_string()));
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
        return Err(DispatchError::action_failed(
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
            &PipelineWaitStatus::Failed,
            "withdrawn",
            &reason,
            &task_statuses,
        )));
    }

    // A hold or admission error on a task that is still admissible is not a
    // withdrawal either, but it fails the gate the same way: the child never
    // launches and `release_reservation` still runs before the run fails.
    if !refused.is_empty() {
        let detail = refused
            .iter()
            .map(|(task_id, status, error)| format!("{task_id}={status}: {error}"))
            .collect::<Vec<_>>()
            .join("; ");
        let reason = format!(
            "task_gate_pipeline ineligible: workflow admission for '{workflow}' refused child dispatch: {detail}"
        );
        record_gate_admission_stop(
            runtime,
            action,
            input,
            &task_ids,
            &task_statuses,
            &reason,
            "admission_refused",
        )?;
        return Ok(Some(gate_admission_stop_output(
            input,
            &PipelineWaitStatus::Failed,
            "admission_refused",
            &reason,
            &task_statuses,
        )));
    }

    if !claimed_statuses.is_empty() {
        let reason = format!(
            "task_gate_pipeline claimed elsewhere: workflow admission for '{workflow}' skipped child \
             dispatch because a live distributed execution claim holds {}; the claim is its one \
             execution",
            summarize_statuses(&claimed_statuses)
        );
        record_gate_admission_stop(
            runtime,
            action,
            input,
            &task_ids,
            &task_statuses,
            &reason,
            "claimed_elsewhere",
        )?;
        return Ok(Some(gate_admission_stop_output(
            input,
            &wait_success_status(),
            "claimed_elsewhere",
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

/// The gate's starvation deferral, when its wait expired [ORB-15514].
fn starvation_deferral(input: &Value) -> Option<&Value> {
    input
        .get("deferral")
        .filter(|deferral| deferral.get("deferred").and_then(Value::as_bool) == Some(true))
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
pub(crate) fn wait_success_status() -> PipelineWaitStatus {
    JobRunState::Success.into()
}

/// Build the synthetic child-run result an admission stop reports in place of a
/// real dispatch. `status` drives the gate: the canonical success token lets
/// `pipeline_success_guard` pass, anything else fails the gate *after*
/// `release_reservation` has run, and `error` is what the guard quotes.
fn gate_admission_stop_output(
    input: &Value,
    status: &PipelineWaitStatus,
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
    if *status != wait_success_status() {
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
/// `stale_noop` (already-shipped work), `withdrawn` (a human moved the task
/// out of automation's reach [ORB-11305]), `admission_refused` (the task kept
/// an admissible status but admission refused it, e.g. a pilot hold; the
/// reason carries the admission error) or `claimed_elsewhere` (a live
/// distributed claim is executing it [ORB-13918]); each is recorded so a run
/// that launched nothing is still explainable from the audit log alone.
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
    let arguments_json = serde_json::to_string(&payload).map_err(|err| {
        DispatchError::action_failed(action, format!("serialize gate.{outcome} payload: {err}"))
    })?;
    let command = format!("gate.{outcome}");
    let task_id = task_ids.first().map(String::as_str);
    record_system_audit_event(
        runtime,
        SystemAuditEvent {
            execution_id_prefix: "audit-gate-admission-stop",
            command: &command,
            subcommand: None,
            tool_name: None,
            target_type: "task_bundle",
            target_id: task_id,
            role: "admin",
            status: AuditEventStatus::Success,
            arguments_json,
            workspace_id: None,
            task_id,
            attribution: AuditRunAttribution::Run(parent_run_id),
        },
    )
    .map_err(|err| {
        DispatchError::action_failed(action, format!("record gate.{outcome} audit: {err}"))
    })
}
