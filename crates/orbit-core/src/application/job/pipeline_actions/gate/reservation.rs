//! The gate's bundle admission: the conflict verdict a gate polls on, and the
//! reservation it takes once dependencies allow.
//!
//! Dispatch admission separates unmet dependencies, which the gate waits on,
//! from unsatisfiable ones, which fail now [ORB-10593].

use std::collections::BTreeSet;

use orbit_common::OrbitError;
use orbit_common::protocol::tool_input::optional_string_list_alias;
use orbit_engine::DispatchError;
use orbit_tools::ToolContext;
use orbit_types::task::{
    TaskReferenceIndex, UnsatisfiableTaskDependency, unmet_task_dependencies_with_index,
    unsatisfiable_task_dependencies_with_index,
};
use serde_json::Value;

use crate::OrbitRuntime;
use crate::application::job::JobAdmissionPort;
use crate::runtime::task::locks::{
    EmptyTaskSurfacePolicy, ReservationOutcome, TaskLockIndex, emit_expired_reservation_events,
    merge_task_lock_conflicts, parse_task_ids, requested_task_files_indexed, reserve_with_index,
    task_lock_conflicts_indexed, workspace_orbit_dir, workspace_task_reservation_id,
};

/// Whether the bundle's footprint is clear: task locks held by other tasks,
/// after reconciling stale owned reservations, plus live store reservations.
/// Expired reservations met on the way are audited.
pub(crate) fn context_conflict_check(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<Value, DispatchError> {
    let task_ids = parse_task_ids(input)
        .map_err(|error| DispatchError::action_failed(action, error.to_string()))?;
    let index = TaskLockIndex::load(runtime, &task_ids)
        .map_err(|error| DispatchError::action_failed(action, error.to_string()))?;
    let repo_root = runtime.paths().repo_root.as_path();
    let requested_files = requested_task_files_indexed(&index, &task_ids, repo_root)
        .map_err(|error| DispatchError::action_failed(action, error.to_string()))?;
    let task_conflicts =
        task_lock_conflicts_indexed(&index, &task_ids, &requested_files, repo_root);
    runtime
        .reconcile_stale_owned_reservations_for_files(&requested_files, 32)
        .map_err(|error| DispatchError::action_failed(action, error.to_string()))?;
    let reservation_check = runtime
        .stores()
        .task_reservations()
        .check_task_reservation_conflicts(orbit_store::contracts::TaskReservationCheckParams {
            workspace_orbit_dir: workspace_orbit_dir(runtime),
            workspace_id: workspace_task_reservation_id(runtime)
                .map_err(|error| DispatchError::action_failed(action, error.to_string()))?,
            requested_files,
        })
        .map_err(|error| DispatchError::action_failed(action, error.to_string()))?;
    emit_expired_reservation_events(runtime, &reservation_check.expired_reservations)
        .map_err(|error| DispatchError::action_failed(action, error.to_string()))?;
    let conflicts = merge_task_lock_conflicts(task_conflicts, reservation_check.conflicts);
    Ok(serde_json::json!({
        "clear": conflicts.is_empty(),
        "conflicts": conflicts,
    }))
}

/// One gate poll: fail on an unsatisfiable dependency, wait on an unmet one,
/// and otherwise try to reserve the bundle's footprint. Each outcome records
/// the run's waiting reasons.
pub(crate) fn reserve_locks(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
    tool_context: &ToolContext,
) -> Result<Value, DispatchError> {
    let admission = dependency_admission_for_input(runtime, input)
        .map_err(|err| DispatchError::action_failed(action, format!("{err}")))?;
    // Fail before the first poll: an archived/rejected/dangling
    // `blocked_by` target cannot be cleared by waiting, so returning
    // `reserved: false` here would burn the gate's whole iteration
    // budget and then report a file conflict that does not exist.
    if !admission.unsatisfiable.is_empty() {
        update_run_waiting_reasons(
            runtime,
            input,
            non_empty(
                admission
                    .unsatisfiable
                    .iter()
                    .map(|dependency| dependency.dependency_id.clone())
                    .collect(),
            ),
            None,
            action,
        )?;
        return Err(DispatchError::action_failed(
            action,
            admission.unsatisfiable_message(),
        ));
    }
    if !admission.waiting_on.is_empty() {
        update_run_waiting_reasons(
            runtime,
            input,
            Some(admission.waiting_on.clone()),
            None,
            action,
        )?;
        return Ok(ReservationOutcome::Denied {
            conflicts: Vec::new(),
        }
        .into_step_output(admission.waiting_on));
    }

    let task_ids = parse_task_ids(input)
        .map_err(|err| DispatchError::action_failed(action, format!("{err}")))?;
    let lock_index = TaskLockIndex::load(runtime, &task_ids)
        .map_err(|err| DispatchError::action_failed(action, format!("{err}")))?;
    let mut output = reserve_with_index(
        runtime,
        input.clone(),
        tool_context.agent_name.clone(),
        tool_context.model_name.clone(),
        tool_context.reservation_owner.clone(),
        &lock_index,
        EmptyTaskSurfacePolicy::Admit,
    )
    .map_err(|err| DispatchError::action_failed(action, format!("{err}")))?;
    // A resumed published delivery adopts before it edits, so its
    // gate does not wait on the holders that refused it; the leaf
    // takes its footprint before its first edit.
    let published_delivery = if let Some((deferred, publication)) = runtime
        .published_delivery_reservation(
            input,
            &output,
            tool_context.agent_name.clone(),
            tool_context.model_name.clone(),
            tool_context.reservation_owner.clone(),
        )
        .map_err(|err| DispatchError::action_failed(action, format!("{err}")))?
    {
        output = deferred;
        Some(publication)
    } else {
        None
    };
    let waiting_on_locks = waiting_locks_from_reserve_output(&output);
    update_run_waiting_reasons(runtime, input, None, non_empty(waiting_on_locks), action)?;
    let mut output = output.into_step_output(Vec::new());
    if let Some(publication) = published_delivery {
        output["published_delivery"] = serde_json::json!(publication);
    }
    Ok(output)
}

/// The dependency picture for a bundle, split by what the caller should do
/// about it.
///
/// `waiting_on` is the poll-and-retry set; `unsatisfiable` is the fail-now set.
/// An edge appears in exactly one of them — every unsatisfiable edge is also
/// unmet, but it is reported only as unsatisfiable so the two causes never
/// blur together in a diagnostic.
#[derive(Debug, Default)]
pub(super) struct BundleDependencyAdmission {
    pub(super) waiting_on: Vec<String>,
    pub(super) unsatisfiable: Vec<UnsatisfiableTaskDependency>,
}

impl BundleDependencyAdmission {
    /// Failure message for the unsatisfiable set. Prefixed with a stable
    /// `task.dependencies.unsatisfiable:` marker so an operator (or a
    /// parent job parsing run errors) can tell this apart from
    /// `gate.starvation`, which means the opposite thing: waiting was
    /// legitimate but ran out of budget.
    pub(super) fn unsatisfiable_message(&self) -> String {
        let labels = self
            .unsatisfiable
            .iter()
            .map(UnsatisfiableTaskDependency::label)
            .collect::<Vec<_>>()
            .join("; ");
        format!(
            "task.dependencies.unsatisfiable: {labels}. \
             These dependency edges can never reach 'done', so waiting cannot clear them \
             (no file-lock conflict is involved). Fix the task graph and re-dispatch."
        )
    }
}

pub(super) fn dependency_admission_for_input(
    runtime: &OrbitRuntime,
    input: &Value,
) -> Result<BundleDependencyAdmission, OrbitError> {
    let Some(raw_task_ids) =
        optional_string_list_alias(input, &["task_ids", "taskIds", "task-ids"])?
    else {
        return Ok(BundleDependencyAdmission::default());
    };
    let task_ids = parse_task_ids(&serde_json::json!({ "task_ids": raw_task_ids }))?;
    let tasks = task_ids
        .iter()
        .map(|task_id| runtime.get_task(task_id))
        .collect::<Result<Vec<_>, _>>()?;
    let status_by_id = runtime.dependency_status_index(&tasks)?;
    let reference_index = TaskReferenceIndex::from_status_index(&status_by_id);
    let mut waiting_on = BTreeSet::new();
    let mut unsatisfiable = Vec::new();
    for task in tasks {
        let dead_ends =
            unsatisfiable_task_dependencies_with_index(&task, &status_by_id, &reference_index);
        let dead_end_ids = dead_ends
            .iter()
            .map(|dependency| dependency.dependency_id.clone())
            .collect::<BTreeSet<_>>();
        for dependency in unmet_task_dependencies_with_index(&task, &status_by_id, &reference_index)
        {
            if !dead_end_ids.contains(&dependency.id) {
                waiting_on.insert(dependency.id);
            }
        }
        unsatisfiable.extend(dead_ends);
    }
    Ok(BundleDependencyAdmission {
        waiting_on: waiting_on.into_iter().collect(),
        unsatisfiable,
    })
}

fn waiting_locks_from_reserve_output(output: &ReservationOutcome) -> Vec<String> {
    let conflicts = match output {
        ReservationOutcome::Granted { .. } => return Vec::new(),
        ReservationOutcome::Denied { conflicts } => conflicts,
    };
    conflicts
        .iter()
        .map(|conflict| conflict.file.trim())
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn update_run_waiting_reasons(
    runtime: &OrbitRuntime,
    input: &Value,
    waiting_on_deps: Option<Vec<String>>,
    waiting_on_locks: Option<Vec<String>>,
    action: &str,
) -> Result<(), DispatchError> {
    let Some(run_id) = input.get("run_id").and_then(Value::as_str) else {
        return Ok(());
    };
    // Same transactional primitive as checkpoints and child dispatch: a
    // separate read then whole-document write would discard a concurrent
    // admissions-stop or worker-limit that landed in between.
    let mut waiting_on_deps = waiting_on_deps;
    let mut waiting_on_locks = waiting_on_locks;
    runtime
        .stores()
        .jobs()
        .update_run_state(run_id, &mut |_, state| {
            state.set_waiting_reasons(waiting_on_deps.take(), waiting_on_locks.take());
            Ok(())
        })
        .map(|_| ())
        .map_err(|err| DispatchError::action_failed(action, format!("{err}")))
}

fn non_empty(values: Vec<String>) -> Option<Vec<String>> {
    (!values.is_empty()).then_some(values)
}
