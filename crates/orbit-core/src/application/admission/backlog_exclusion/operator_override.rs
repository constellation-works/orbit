//! An operator's explicit ship overrides the drain's readiness gates and
//! local lock waits [ORB-25082].
//!
//! Readiness gates and lock waits are automatic-drain policy: they decide
//! which backlog work a drain may pick on its own and when. When an operator
//! ships a named task from the CLI (`orbit run ship <id>`) or the dashboard's
//! Ship, that choice overrides them, and the task dispatches:
//!
//! - `scheduled` — a `not_before` still ahead (the override also writes the
//!   `schedule_overridden` event every other admission route reads);
//! - `unassessed_complexity` — no complexity assessment;
//! - `unknown_footprint` — no `context_files`, which a multi-slot drain holds
//!   for the task pilot and then runs alone (`awaiting_footprint`,
//!   `awaiting_exclusive_slot`, or a whole-tree conflict);
//! - `context_lock_conflict` — its gate's reservation overlaps the locks of
//!   another in-flight task or reservation. The gate grants a reservation
//!   holding no files instead of waiting, and records the holders it
//!   overrode; once the task is `in-progress` its own context locks hold
//!   against every later drain admission.
//!
//! What makes the run impossible or unsafe here still refuses it: host OS,
//! `requires_labels`, an unverified browser kit for a `ui` task, a human crew,
//! a live run or execution claim already holding the task, an active
//! task-pilot preparation, the task pilot's findings (they have their own
//! operator release), an inherited-only epic root (every reservation refuses
//! one), and a pull follower's execution claim over its footprint, which the
//! store's work-start guard refuses on every route.

use std::collections::BTreeMap;

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::task::{Task, TaskHistoryEntry, declares_no_footprint};

use super::exclusions::clears_complexity_gate;
use crate::OrbitRuntime;
use crate::application::task::{TaskRecordUpdateParams, TaskRecords};
use orbit_store::contracts::{TaskLockConflict, TaskLockHolder};

use crate::runtime::task::locks::{workspace_orbit_dir, workspace_task_reservation_id};

/// The history event recording which readiness gates and lock conflicts an
/// operator's ship overrode.
const READINESS_OVERRIDDEN_EVENT: &str = "readiness_overridden";

/// The readiness gates `task` fails now that an operator's explicit ship
/// overrides, in a stable order. `schedule_held` is whether its `not_before`
/// still holds it.
pub(in crate::application::admission) fn operator_overridden_gates(
    task: &Task,
    schedule_held: bool,
) -> Vec<String> {
    let mut gates = Vec::new();
    if schedule_held {
        gates.push("scheduled".to_string());
    }
    if !clears_complexity_gate(task) {
        gates.push("unassessed_complexity".to_string());
    }
    if declares_no_footprint(&task.tags, &task.context_files) {
        gates.push("unknown_footprint".to_string());
    }
    gates
}

/// The `context_lock_conflict` gate an operator's ship overrode at its gate,
/// naming each holder, the tasks a reservation holds the files for, and the
/// files it held, ordered by holder. A reservation that can no longer be read
/// is named by its id alone.
pub(in crate::application::admission) fn lock_conflict_gate(
    runtime: &OrbitRuntime,
    conflicts: &[TaskLockConflict],
) -> String {
    let mut holders: BTreeMap<(&'static str, &str), Vec<&str>> = BTreeMap::new();
    for conflict in conflicts {
        let kind = match conflict.held_by {
            TaskLockHolder::Task => "task",
            TaskLockHolder::Reservation => "reservation",
            TaskLockHolder::Lease => "lease",
        };
        let files = holders
            .entry((kind, conflict.held_by_id.as_str()))
            .or_default();
        if !files.contains(&conflict.file.as_str()) {
            files.push(conflict.file.as_str());
        }
    }
    let named = holders
        .into_iter()
        .map(|((kind, id), files)| {
            let tasks = if kind == "reservation" {
                reservation_task_ids(runtime, id)
                    .map(|tasks| format!(" for {}", tasks.join(", ")))
                    .unwrap_or_default()
            } else {
                String::new()
            };
            format!("{kind} {id}{tasks} on {}", files.join(", "))
        })
        .collect::<Vec<_>>();
    format!("context_lock_conflict ({})", named.join("; "))
}

/// The tasks an active reservation holds files for, when it can be read and
/// names any.
fn reservation_task_ids(runtime: &OrbitRuntime, reservation_id: &str) -> Option<Vec<String>> {
    let workspace_id = workspace_task_reservation_id(runtime).ok()?;
    let reservation = runtime
        .stores()
        .task_reservations()
        .inspect_active_task_reservation(
            &workspace_orbit_dir(runtime),
            workspace_id.as_deref(),
            reservation_id,
        )
        .inspect_err(|error| {
            tracing::warn!(
                reservation_id,
                "could not read an overridden reservation: {error}"
            );
        })
        .ok()??;
    (!reservation.task_ids.is_empty()).then_some(reservation.task_ids)
}

/// Record that `actor`'s explicit ship of `task_id` overrode `gates`.
pub(in crate::application::admission) fn record_readiness_override(
    runtime: &OrbitRuntime,
    task_id: &str,
    gates: &[String],
    actor: &str,
) -> Result<(), OrbitError> {
    runtime.ensure_coordination_task_write_permitted()?;
    runtime.stores().task_records().update(
        task_id,
        TaskRecordUpdateParams {
            actor: actor.to_string(),
            append_history: vec![TaskHistoryEntry {
                at: Utc::now(),
                by: actor.to_string(),
                event: READINESS_OVERRIDDEN_EVENT.to_string(),
                note: Some(format!(
                    "operator `{actor}` shipped it explicitly, overriding the drain's \
                     readiness gates: {}",
                    gates.join(", ")
                )),
                from_status: None,
                to_status: None,
                operation_id: None,
                decision: None,
            }],
            ..Default::default()
        },
    )?;
    Ok(())
}
