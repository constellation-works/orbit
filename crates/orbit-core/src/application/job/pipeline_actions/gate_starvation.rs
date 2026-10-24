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
use orbit_store::contracts::{TaskLockConflict, TaskLockHolder};
use orbit_tools::ToolContext;
use orbit_types::policy::Role;
use orbit_types::telemetry::AuditEventStatus;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::admission::claim_holders::ClaimHolders;
use crate::runtime::audit::system::{
    AuditRunAttribution, SystemAuditEvent, record_system_audit_event,
};
use crate::runtime::task::locks::{workspace_orbit_dir, workspace_task_reservation_id};

use super::gate_reservation::{context_conflict_check, dependency_admission_for_input};

/// Parsed once where the rendered step input enters the dispatcher.
#[derive(Deserialize)]
struct GateStarvationInput {
    #[serde(default, deserialize_with = "string_list")]
    task_ids: Vec<String>,
    #[serde(default)]
    expired: bool,
    #[serde(default)]
    reservation: GateReservationInput,
    #[serde(flatten)]
    window: GateWindow,
    max_wait_seconds: Option<f64>,
    run_id: Option<String>,
}

/// Absence is the legacy flat input; null means the loop never polled.
#[derive(Default)]
enum GateReservationInput {
    #[default]
    Missing,
    Unpolled,
    Observed(Box<GateReservation>),
}

impl<'de> Deserialize<'de> for GateReservationInput {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Option::<GateReservation>::deserialize(deserializer)?
            .map(|reservation| Self::Observed(Box::new(reservation)))
            .unwrap_or(Self::Unpolled))
    }
}

/// Preserve unconsumed grant metadata in the persisted reservation, including
/// deferred footprints and publication details, even after releasing a late grant.
#[derive(Deserialize, Serialize)]
struct GateReservation {
    #[serde(flatten)]
    window: GateWindow,
    #[serde(skip_serializing_if = "Option::is_none")]
    reservation_id: Option<String>,
    #[serde(flatten)]
    metadata: serde_json::Map<String, Value>,
}

impl GateReservation {
    fn unpolled(conflicts: Vec<TaskLockConflict>, waiting_on_deps: Vec<String>) -> Self {
        Self {
            window: GateWindow {
                reserved: Some(TemplateReserved::Bool(false)),
                conflicts: Some(conflicts),
                waiting_on_deps: Some(waiting_on_deps),
            },
            reservation_id: None,
            metadata: serde_json::Map::from_iter([(
                "footprint_deferred".to_string(),
                json!(false),
            )]),
        }
    }
}

#[derive(Clone, Default, Deserialize, Serialize)]
struct GateWindow {
    #[serde(skip_serializing_if = "Option::is_none")]
    reserved: Option<TemplateReserved>,
    #[serde(skip_serializing_if = "Option::is_none")]
    conflicts: Option<Vec<TaskLockConflict>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    waiting_on_deps: Option<Vec<String>>,
}

/// Templates can render the success flag as either a boolean or text. Keep
/// its wire representation while interpreting it only at this boundary.
#[derive(Clone, Deserialize, Serialize)]
#[serde(untagged)]
enum TemplateReserved {
    Bool(bool),
    Text(String),
    Other(Value),
}

impl Default for TemplateReserved {
    fn default() -> Self {
        Self::Other(Value::Null)
    }
}

impl TemplateReserved {
    fn granted(&self) -> bool {
        match self {
            Self::Bool(value) => *value,
            Self::Text(value) => value.trim().parse().unwrap_or(false),
            Self::Other(_) => false,
        }
    }
}

/// The gate's post-wait step [ORB-15514]. A gate that never polled observes
/// its blockers here without acquiring a late reservation; an expired window
/// whose late poll was granted releases that grant through the audited
/// `orbit.task.locks.release` tool before deferring.
pub(crate) fn gate_starvation_step(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
    tool_context: ToolContext,
) -> Result<Value, DispatchError> {
    let mut gate: GateStarvationInput = serde_json::from_value(input.clone())
        .map_err(|err| DispatchError::action_failed(action, format!("parse gate input: {err}")))?;
    match &gate.reservation {
        GateReservationInput::Unpolled => {
            // A zero/expired budget observes blockers without ever
            // acquiring a late reservation.
            let admission = dependency_admission_for_input(runtime, input)
                .map_err(|err| DispatchError::action_failed(action, err.to_string()))?;
            if !admission.unsatisfiable.is_empty() {
                return Err(DispatchError::action_failed(
                    action,
                    admission.unsatisfiable_message(),
                ));
            }
            // Errors keep naming the conflict-check action this step used to
            // dispatch through.
            let conflicts = context_conflict_check(runtime, "context_conflict_check", input)?;
            #[derive(Deserialize)]
            struct ConflictCheckOutput {
                conflicts: Vec<TaskLockConflict>,
            }
            let conflicts: ConflictCheckOutput = serde_json::from_value(conflicts)
                .map_err(|err| DispatchError::action_failed(action, err.to_string()))?;
            gate.reservation = GateReservationInput::Observed(Box::new(GateReservation::unpolled(
                conflicts.conflicts,
                admission.waiting_on,
            )));
        }
        GateReservationInput::Missing | GateReservationInput::Observed(_) => {}
    }
    if let GateReservationInput::Observed(reservation) = &gate.reservation {
        gate.window = reservation.window.clone();
    }
    // Only a boolean grant authorizes late-grant cleanup. Legacy
    // text flags retain the template boundary's existing interpretation.
    if gate.expired && matches!(gate.window.reserved, Some(TemplateReserved::Bool(true))) {
        let reservation_id = match &gate.reservation {
            GateReservationInput::Observed(reservation) => reservation.reservation_id.as_deref(),
            _ => None,
        }
        .ok_or_else(|| {
            DispatchError::action_failed(action, "late reservation missing reservation_id")
        })?;
        runtime
            .run_tool_with_context_and_role(
                "orbit.task.locks.release",
                json!({"reservation_id": reservation_id}),
                Role::Admin,
                tool_context,
            )
            .map_err(|err| DispatchError::action_failed("release_locks", format!("{err}")))?;
        gate.window.reserved = Some(TemplateReserved::Bool(false));
        if let GateReservationInput::Observed(reservation) = &mut gate.reservation {
            reservation.window.reserved = Some(TemplateReserved::Bool(false));
        }
    }
    let mut output = gate_starvation_defer(runtime, action, &gate)?;
    output["reservation"] = match gate.reservation {
        GateReservationInput::Observed(reservation) => json!(reservation),
        _ => json!({}),
    };
    Ok(output)
}

/// The gate's post-wait step. Runs on every gate run so later steps may read
/// its output: a reserved window is `{deferred: false}`; an expired one is
/// audited as `gate.starvation` and returned as the deferral `invoke_and_wait`
/// stops on.
fn gate_starvation_defer(
    runtime: &OrbitRuntime,
    action: &str,
    input: &GateStarvationInput,
) -> Result<Value, DispatchError> {
    if input
        .window
        .reserved
        .as_ref()
        .is_some_and(TemplateReserved::granted)
    {
        return Ok(json!({ "deferred": false }));
    }
    let task_ids = &input.task_ids;
    let conflicts = input.window.conflicts.as_deref().unwrap_or_default();
    let max_wait_seconds = input.max_wait_seconds;
    let conflicting_files = conflicts
        .iter()
        .map(|conflict| conflict.file.clone())
        .collect::<BTreeSet<_>>();
    let waiting_on_deps = input
        .window
        .waiting_on_deps
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|dependency| dependency.trim())
        .filter(|dependency| !dependency.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    let mut blocking_task_ids = blocking_lock_holders(runtime, action, conflicts)?;
    blocking_task_ids.extend(waiting_on_deps.iter().cloned());
    for task_id in task_ids {
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
    record_starvation(
        runtime,
        action,
        input.run_id.as_deref(),
        task_ids,
        &deferral,
    )?;
    Ok(deferral)
}

fn string_list<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<String>, D::Error> {
    let value = Value::deserialize(deserializer)?;
    Ok(value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(ToOwned::to_owned)
        .collect())
}

/// The tasks behind the last conflicts: a task lock names its task, and a
/// reservation the tasks it was taken for. A reservation released since the
/// last poll names nobody.
fn blocking_lock_holders(
    runtime: &OrbitRuntime,
    action: &str,
    conflicts: &[TaskLockConflict],
) -> Result<BTreeSet<String>, DispatchError> {
    let mut blocking = BTreeSet::new();
    let mut reservations = BTreeSet::new();
    for conflict in conflicts {
        match conflict.held_by {
            TaskLockHolder::Task => {
                blocking.insert(conflict.held_by_id.clone());
            }
            TaskLockHolder::Reservation => {
                reservations.insert(conflict.held_by_id.clone());
            }
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
    run_id: Option<&str>,
    task_ids: &[String],
    deferral: &Value,
) -> Result<(), DispatchError> {
    let parent_run_id = run_id
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
