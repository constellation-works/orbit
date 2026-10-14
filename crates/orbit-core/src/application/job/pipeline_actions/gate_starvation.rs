//! Gate starvation deferral [ORB-15514].
//!
//! A gate whose wait budget runs out did nothing wrong: another task held its
//! footprint, or a dependency had not landed, for longer than it waits. It
//! used to fail its run, which reported healthy work as broken and hid the
//! one fact that explains the wait. It now defers: the task stays `backlog`
//! for the next drain pass, the run ends without dispatching a child, and the
//! deferral names the blocking tasks and any execution claim holding them.

use std::collections::BTreeSet;

use orbit_engine::DispatchError;
use orbit_types::telemetry::AuditEventStatus;
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::admission::claim_holders::ClaimHolders;
use crate::runtime::audit::system::{
    AuditRunAttribution, SystemAuditEvent, record_system_audit_event,
};
use crate::runtime::task::locks::{workspace_orbit_dir, workspace_task_reservation_id};

/// The gate's post-wait step. Runs on every gate run so later steps may read
/// its output: a reserved window is `{deferred: false}`; an expired one is
/// audited as `gate.starvation` and returned as the deferral `invoke_and_wait`
/// stops on.
pub(crate) fn gate_starvation_defer(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<Value, DispatchError> {
    if reserved(input) {
        return Ok(json!({ "deferred": false }));
    }
    let task_ids = string_list(input.get("task_ids"));
    let conflicts = input
        .get("conflicts")
        .cloned()
        .unwrap_or(Value::Array(Vec::new()));
    let max_wait_seconds = input.get("max_wait_seconds").and_then(Value::as_f64);
    let conflicting_files = conflict_entries(&conflicts)
        .filter_map(|entry| entry.get("file").and_then(Value::as_str))
        .map(ToOwned::to_owned)
        .collect::<BTreeSet<_>>();
    // The gate can starve on either axis. Reporting only `conflicting_files`
    // left a dependency-starved bundle with an empty list and no blocker
    // named at all, so carry the last-observed unmet dependency IDs too.
    let waiting_on_deps = string_list(input.get("waiting_on_deps"));
    let mut blocking_task_ids = blocking_lock_holders(runtime, action, &conflicts)?;
    blocking_task_ids.extend(waiting_on_deps.iter().cloned());
    for task_id in &task_ids {
        blocking_task_ids.remove(task_id);
    }
    let claim_holders = ClaimHolders::read(runtime);
    let blocking_claims = claim_holders.json_for(blocking_task_ids.iter().map(String::as_str));

    let mut detail = format!(
        "admission window never opened within {}; deferred to backlog",
        max_wait_seconds
            .map(|seconds| format!("{seconds}s"))
            .unwrap_or_else(|| "the wait budget".to_string())
    );
    if !blocking_task_ids.is_empty() {
        detail.push_str(&format!(
            ", blocked by {}",
            blocking_task_ids
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if let Some(claims) = claim_holders.detail_for(blocking_task_ids.iter().map(String::as_str)) {
        detail.push_str(&format!(" ({claims})"));
    }

    let deferral = json!({
        "deferred": true,
        "reason": "gate_starvation",
        "task_ids": task_ids,
        "blocking_task_ids": blocking_task_ids,
        "blocking_claims": blocking_claims,
        "conflicting_files": conflicting_files,
        "conflicts": conflicts,
        "waiting_on_deps": waiting_on_deps,
        "max_wait_seconds": max_wait_seconds,
        "detail": detail,
    });
    record_starvation(runtime, action, input, &task_ids, &deferral)?;
    Ok(deferral)
}

/// Whether the wait loop's last reservation succeeded. The template renders
/// the flag as a bool or, inside a larger string, as its text.
fn reserved(input: &Value) -> bool {
    match input.get("reserved") {
        Some(Value::Bool(reserved)) => *reserved,
        Some(Value::String(reserved)) => reserved.trim() == "true",
        _ => false,
    }
}

fn string_list(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| entry.as_str().map(str::trim))
                .filter(|entry| !entry.is_empty())
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn conflict_entries(conflicts: &Value) -> impl Iterator<Item = &Value> {
    conflicts.as_array().into_iter().flatten()
}

/// The tasks behind the last conflicts: a task lock names its task, and a
/// reservation the tasks it was taken for. A reservation released since the
/// last poll names nobody.
fn blocking_lock_holders(
    runtime: &OrbitRuntime,
    action: &str,
    conflicts: &Value,
) -> Result<BTreeSet<String>, DispatchError> {
    let mut blocking = BTreeSet::new();
    let mut reservations = BTreeSet::new();
    for conflict in conflict_entries(conflicts) {
        let Some(holder) = conflict.get("held_by_id").and_then(Value::as_str) else {
            continue;
        };
        match conflict.get("held_by").and_then(Value::as_str) {
            Some("task") => {
                blocking.insert(holder.to_string());
            }
            Some("reservation") => {
                reservations.insert(holder.to_string());
            }
            _ => {}
        }
    }
    if reservations.is_empty() {
        return Ok(blocking);
    }
    let workspace_id = workspace_task_reservation_id(runtime).map_err(|err| {
        DispatchError::action_failed(action, format!("resolve reservation workspace: {err}"))
    })?;
    for reservation_id in reservations {
        let reservation = runtime
            .stores()
            .task_reservations()
            .inspect_active_task_reservation(
                &workspace_orbit_dir(runtime),
                workspace_id.as_deref(),
                &reservation_id,
            )
            .map_err(|err| {
                DispatchError::action_failed(
                    action,
                    format!("read reservation {reservation_id}: {err}"),
                )
            })?;
        if let Some(reservation) = reservation {
            blocking.extend(reservation.task_ids);
        }
    }
    Ok(blocking)
}

/// A deferral is the gate working as designed, so it audits as a success
/// whose payload says why nothing ran.
fn record_starvation(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
    task_ids: &[String],
    deferral: &Value,
) -> Result<(), DispatchError> {
    let parent_run_id = input
        .get("run_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let mut payload = deferral.clone();
    payload["outcome"] = json!("deferred");
    let arguments_json = serde_json::to_string(&payload).map_err(|err| {
        DispatchError::action_failed(action, format!("serialize gate.starvation payload: {err}"))
    })?;
    let task_id = task_ids.first().map(String::as_str);
    record_system_audit_event(
        runtime,
        SystemAuditEvent {
            execution_id_prefix: "audit-gate-starvation",
            command: "gate.starvation",
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
        DispatchError::action_failed(action, format!("record gate.starvation audit: {err}"))
    })
}
