//! Locked, retried, idempotent persistence of one validated task-pilot assessment.

use orbit_common::OrbitError;
use orbit_store::contracts::{AtomicTaskMutationOutcome, AtomicTaskMutationParams};
use orbit_types::record::OrbitEvent;
use orbit_types::task::{Task, TaskStatus};
use orbit_types::workflow::automation::members::PreparationEligibility;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::OrbitRuntime;
#[cfg(test)]
use crate::application::task::TaskUpdateParams;

use super::apply::{PreparedTaskSnapshot, ValidatedTask};

const STORAGE_APPLY_ATTEMPTS: usize = 3;

pub(super) enum ApplyTaskOutcome {
    Applied(Option<String>),
    AlreadyApplied(Option<String>),
    Stale(&'static str, &'static str),
}

pub(super) fn apply_task(
    runtime: &OrbitRuntime,
    snapshot: &PreparedTaskSnapshot,
    task: &ValidatedTask,
    prepared: &Value,
    eligibility: &PreparationEligibility,
) -> Result<ApplyTaskOutcome, OrbitError> {
    if !matches!(snapshot.status, TaskStatus::Proposed | TaskStatus::Backlog) {
        return Ok(ApplyTaskOutcome::Stale(
            "status_not_mutable",
            "task-pilot does not rewrite in-progress, review, or terminal work",
        ));
    }
    let mut snapshot = snapshot.clone();
    for attempt in 0..=1 {
        let mut lock_ids = vec![task.task_id.clone()];
        lock_ids.extend(runtime.get_task(&task.task_id)?.dependencies());
        lock_ids.sort();
        lock_ids.dedup();
        let mut outcome = None;
        let mut retry_fingerprint = None;
        let mut operation = || {
            crate::application::automation::members::claim(runtime, prepared)?;
            let receipt = format!("operation_id={}", task.operation_id);
            if runtime
                .get_task_history(&task.task_id)?
                .iter()
                .any(|event| {
                    event.event == "task_pilot_applied"
                        && event
                            .note
                            .as_deref()
                            .is_some_and(|note| note.lines().next() == Some(receipt.as_str()))
                })
            {
                outcome = Some(ApplyTaskOutcome::AlreadyApplied(resulting_fingerprint(
                    runtime,
                    &task.task_id,
                    &snapshot,
                    eligibility,
                )?));
                return Ok(());
            }
            let current = match runtime.get_task(&task.task_id) {
                Ok(current) => current,
                Err(OrbitError::NotFound { .. }) => {
                    outcome = Some(ApplyTaskOutcome::Stale(
                        "task_deleted",
                        "task no longer exists at the write boundary",
                    ));
                    return Ok(());
                }
                Err(error) => return Err(error),
            };
            if let Some(reason) = task_snapshot_drift(runtime, &current, &snapshot, eligibility) {
                if attempt == 0 && matches!(reason.0, "material_changed" | "status_changed") {
                    retry_fingerprint =
                        status_only_fingerprint(runtime, &current, &snapshot, eligibility)
                            .map(|fingerprint| (fingerprint, current.status));
                }
                if retry_fingerprint.is_none() {
                    outcome = Some(ApplyTaskOutcome::Stale(reason.0, reason.1));
                }
                return Ok(());
            }
            if attempt == 1 && !matches!(current.status, TaskStatus::Proposed | TaskStatus::Backlog)
            {
                outcome = Some(ApplyTaskOutcome::Stale(
                    "status_changed",
                    "task status changed after preparation; task-pilot does not rewrite active work",
                ));
                return Ok(());
            }

            let target_status = if task.promote {
                TaskStatus::Backlog
            } else {
                snapshot.status
            };
            let mutation_params = AtomicTaskMutationParams {
                actor: "task-pilot".to_string(),
                operation_id: task.operation_id.clone(),
                expected_context_files: snapshot.context_files.clone(),
                expected_status: snapshot.status,
                expected_complexity: snapshot.complexity,
                context_files: task.after.clone(),
                status: target_status,
                complexity: task.complexity,
                event_type: "task_pilot_applied".to_string(),
                event_note: "task-pilot atomic application".to_string(),
                audit_note: serde_json::to_string(&json!({
                    "assessment": task.assessment,
                    "context_files_before": snapshot.context_files,
                    "complexity_before": snapshot.complexity,
                    "complexity_after": task.complexity,
                }))
                .map_err(|error| {
                    OrbitError::Execution(format!("serialize task-pilot audit: {error}"))
                })?,
            };
            let mutation = apply_atomic_with_retries(runtime, &task.task_id, &mutation_params)?;
            match mutation {
                AtomicTaskMutationOutcome::Applied => {
                    runtime.record_event(OrbitEvent::TaskUpdated {
                        id: task.task_id.clone(),
                    })?;
                    outcome = Some(ApplyTaskOutcome::Applied(resulting_fingerprint(
                        runtime,
                        &task.task_id,
                        &snapshot,
                        eligibility,
                    )?));
                }
                AtomicTaskMutationOutcome::AlreadyApplied => {
                    outcome = Some(ApplyTaskOutcome::AlreadyApplied(resulting_fingerprint(
                        runtime,
                        &task.task_id,
                        &snapshot,
                        eligibility,
                    )?));
                }
                AtomicTaskMutationOutcome::Stale => {
                    outcome = Some(ApplyTaskOutcome::Stale(
                        "write_boundary_changed",
                        "task changed between validation and the atomic write boundary",
                    ));
                }
            }
            Ok(())
        };
        with_task_locks(runtime, &lock_ids, 0, &mut operation)?;
        if let Some((fingerprint, status)) = retry_fingerprint {
            // Admit this fresh fingerprint only once. The next pass releases
            // and reacquires the task/dependency locks, then reads them again.
            snapshot.material = snapshot
                .material
                .map(|(_, revision)| (fingerprint, revision));
            snapshot.status = status;
            inject_concurrent_retry_edit(runtime, &task.task_id)?;
            continue;
        }
        return outcome
            .ok_or_else(|| OrbitError::Execution("task-pilot operation did not run".to_string()));
    }
    Err(OrbitError::Execution(
        "task-pilot status retry loop did not settle".to_string(),
    ))
}

fn status_only_fingerprint(
    runtime: &OrbitRuntime,
    current: &Task,
    snapshot: &PreparedTaskSnapshot,
    eligibility: &PreparationEligibility,
) -> Option<String> {
    let (_, revision) = snapshot.material.as_ref()?;
    let neutral = snapshot.status_neutral_fingerprint.as_ref()?;
    let (fresh, fresh_neutral) = crate::application::automation::preparation::fingerprints(
        runtime,
        current,
        revision,
        eligibility,
    )
    .ok()?;
    if &fresh_neutral != neutral {
        return None;
    }
    Some(fresh)
}

fn apply_atomic_with_retries(
    runtime: &OrbitRuntime,
    task_id: &str,
    params: &AtomicTaskMutationParams,
) -> Result<AtomicTaskMutationOutcome, OrbitError> {
    let mut last_error = None;
    for attempt in 0..STORAGE_APPLY_ATTEMPTS {
        match runtime
            .stores()
            .tasks()
            .apply_atomic_task_mutation(task_id, params)
        {
            Ok(outcome) => return Ok(outcome),
            Err(error @ OrbitError::Io(_)) if attempt + 1 < STORAGE_APPLY_ATTEMPTS => {
                last_error = Some(error);
            }
            Err(error) => return Err(error),
        }
    }
    Err(last_error.unwrap_or_else(|| {
        OrbitError::Execution("task-pilot storage retry loop did not run".to_string())
    }))
}

fn resulting_fingerprint(
    runtime: &OrbitRuntime,
    task_id: &str,
    snapshot: &PreparedTaskSnapshot,
    eligibility: &PreparationEligibility,
) -> Result<Option<String>, OrbitError> {
    let Some((_, revision)) = &snapshot.material else {
        return Ok(None);
    };
    let current = runtime.get_task(task_id)?;
    crate::application::automation::preparation::fingerprint(
        runtime,
        &current,
        revision,
        eligibility,
    )
    .map(Some)
    .map_err(orbit_automation::automation_error_to_orbit)
}

pub(super) fn task_operation_id(prepared: &Value, task_id: &str, assessment: &Value) -> String {
    let identity = json!({
        "task_id": task_id,
        "source": prepared.get("source"),
        "prepared_tasks": prepared.get("tasks"),
        "assessment": assessment,
    });
    let encoded = serde_json::to_vec(&identity).unwrap_or_default();
    format!("{:x}", Sha256::digest(encoded))
}

pub(super) fn record_applied_assessment(
    mut task: ValidatedTask,
    snapshot: &PreparedTaskSnapshot,
    outcome: &str,
    task_results: &mut Vec<Value>,
    ci_sweep_admission: &mut Vec<Value>,
) {
    let changed =
        snapshot.context_files != task.after || snapshot.complexity != Some(task.complexity);
    if let Value::Object(fields) = &mut task.assessment {
        fields.insert("applied".to_string(), Value::Bool(changed || task.promote));
        fields.insert("outcome".to_string(), json!(outcome));
        fields.insert("operation_id".to_string(), json!(task.operation_id));
        if let Some(admission) = task.admission.clone() {
            fields.insert("ci_sweep_admission".to_string(), admission);
        }
    }
    if let Some(admission) = task.admission {
        ci_sweep_admission.push(admission);
    }
    task_results.push(task.assessment);
}

fn with_task_locks(
    runtime: &OrbitRuntime,
    task_ids: &[String],
    index: usize,
    operation: &mut dyn FnMut() -> Result<(), OrbitError>,
) -> Result<(), OrbitError> {
    let Some(task_id) = task_ids.get(index) else {
        return operation();
    };
    let mut nested = || with_task_locks(runtime, task_ids, index + 1, operation);
    runtime
        .stores()
        .tasks()
        .with_task_write_lock(task_id, &mut nested)
}

fn task_snapshot_drift(
    runtime: &OrbitRuntime,
    current: &Task,
    snapshot: &PreparedTaskSnapshot,
    eligibility: &PreparationEligibility,
) -> Option<(&'static str, &'static str)> {
    if snapshot
        .material
        .as_ref()
        .is_some_and(|(expected, revision)| {
            crate::application::automation::preparation::fingerprint(
                runtime,
                current,
                revision,
                eligibility,
            )
            .map_or(true, |fingerprint| &fingerprint != expected)
        })
    {
        Some((
            "material_changed",
            "task meaning or dependency evidence changed after preparation",
        ))
    } else if current.context_files != snapshot.context_files {
        Some((
            "context_files_changed",
            "task context_files changed after preparation",
        ))
    } else if current.status != snapshot.status {
        Some(("status_changed", "task status changed after preparation"))
    } else if current.complexity != snapshot.complexity {
        Some((
            "complexity_changed",
            "task complexity changed after preparation",
        ))
    } else if current.title != snapshot.title {
        Some(("title_changed", "task title changed after preparation"))
    } else if current.tags != snapshot.tags {
        Some(("tags_changed", "task tags changed after preparation"))
    } else {
        None
    }
}

pub(super) fn stale_task(task_id: &str, reason: &str, detail: &str) -> Value {
    json!({
        "task_id": task_id,
        "outcome": "stale",
        "reason": reason,
        "detail": detail,
    })
}

pub(super) fn task_outcome(task_id: &str, outcome: &str, error: Option<String>) -> Value {
    json!({ "task_id": task_id, "outcome": outcome, "error": error })
}

#[cfg(test)]
thread_local! {
    static INJECT_CONCURRENT_EDIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static INJECT_RETRY_EDIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(in super::super) fn inject_concurrent_edit_before_locked_apply() {
    INJECT_CONCURRENT_EDIT.set(true);
}

#[cfg(test)]
pub(in super::super) fn inject_concurrent_edit_before_status_retry() {
    INJECT_RETRY_EDIT.set(true);
}

#[cfg(test)]
fn inject_concurrent_retry_edit(runtime: &OrbitRuntime, task_id: &str) -> Result<(), OrbitError> {
    if INJECT_RETRY_EDIT.replace(false) {
        runtime.update_task(
            task_id,
            TaskUpdateParams {
                title: Some("Changed between status reads".to_string()),
                ..TaskUpdateParams::default()
            },
        )?;
    }
    Ok(())
}

#[cfg(not(test))]
fn inject_concurrent_retry_edit(_runtime: &OrbitRuntime, _task_id: &str) -> Result<(), OrbitError> {
    Ok(())
}

#[cfg(test)]
pub(super) fn inject_concurrent_edit(
    runtime: &OrbitRuntime,
    task_id: &str,
) -> Result<(), OrbitError> {
    if INJECT_CONCURRENT_EDIT.replace(false) {
        runtime.update_task(
            task_id,
            TaskUpdateParams {
                tags: Some(vec!["concurrent-edit".to_string()]),
                ..TaskUpdateParams::default()
            },
        )?;
    }
    Ok(())
}

#[cfg(not(test))]
pub(super) fn inject_concurrent_edit(
    _runtime: &OrbitRuntime,
    _task_id: &str,
) -> Result<(), OrbitError> {
    Ok(())
}

pub(super) fn failed_partition(partition_index: u64, task_ids: &[String], error: String) -> Value {
    let task_outcomes = task_ids
        .iter()
        .map(|task_id| task_outcome(task_id, "invalid", Some(error.clone())))
        .collect::<Vec<_>>();
    json!({
        "partition_index": partition_index,
        "task_ids": task_ids,
        "outcome": "failed",
        "error": error,
        "task_outcomes": task_outcomes,
        "unresolved_count": task_ids.len(),
        "applied_task_ids": [],
    })
}
