//! Atomic, retried task mutation; fingerprints, operation ids, recorded assessments and task locks.

use orbit_common::security::release::sha256_hex;
use orbit_common::{OrbitError, StorageLayer};
use orbit_store::contracts::{AtomicTaskMutationOutcome, AtomicTaskMutationParams};
use orbit_types::workflow::automation::members::PreparationPolicy;
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::task::TaskAutomationPort;

use super::super::apply::admission::{
    Admission, AppliedTaskMaterial, PreparedTaskSnapshot, ValidatedTask,
};

const STORAGE_APPLY_ATTEMPTS: usize = 3;

pub(super) fn apply_atomic_with_retries(
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
            Err(error)
                if error.storage_layer() == Some(StorageLayer::Io)
                    && attempt + 1 < STORAGE_APPLY_ATTEMPTS =>
            {
                last_error = Some(error);
            }
            Err(error) => return Err(error),
        }
    }
    Err(last_error.unwrap_or_else(|| {
        OrbitError::Execution("task-pilot storage retry loop did not run".to_string())
    }))
}

pub(super) fn resulting_fingerprint(
    runtime: &OrbitRuntime,
    task_id: &str,
    snapshot: &PreparedTaskSnapshot,
    policy: &PreparationPolicy,
) -> Result<Option<String>, OrbitError> {
    let Some((_, revision)) = &snapshot.material else {
        return Ok(None);
    };
    let current = runtime.get_task(task_id)?;
    runtime
        .task_material_fingerprint(&current, revision, policy)
        .map(Some)
        .map_err(orbit_automation::automation_error_to_orbit)
}

/// Read only the output sealed by this operation. A legacy audit without it
/// cannot authorize approval over a crew redraw or tag change on replay.
pub(super) fn applied_material(
    runtime: &OrbitRuntime,
    task_id: &str,
    operation_id: &str,
) -> Result<Option<AppliedTaskMaterial>, OrbitError> {
    // Oldest match. `pilot_receipts` is newest first; reversing it preserves
    // the previous selection when two comments share an operation id. A bad
    // id is not rejected here, and an undecodable body fails the read.
    let comments = runtime.get_task_comments(task_id)?;
    for (_, receipt) in super::super::pilot_receipts(&comments).rev() {
        if receipt.operation_id != operation_id {
            continue;
        }
        let audit: Value = serde_json::from_str(receipt.body)
            .map_err(|error| OrbitError::Execution(format!("read task-pilot audit: {error}")))?;
        return audit
            .get("applied_material")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|error| {
                OrbitError::Execution(format!("read task-pilot applied material: {error}"))
            });
    }
    Ok(None)
}

pub(in super::super) fn task_operation_id(
    prepared: &Value,
    task_id: &str,
    assessment: &Value,
) -> String {
    let mut identity = json!({
        "task_id": task_id,
        "source": prepared.get("source"),
        "prepared_tasks": prepared.get("tasks"),
        "assessment": assessment,
    });
    // Replay receipts predate insertion-ordered plugin JSON.
    identity.sort_all_objects();
    let encoded = serde_json::to_vec(&identity).unwrap_or_default();
    sha256_hex(&encoded)
}

pub(in super::super) fn record_applied_assessment(
    mut task: ValidatedTask,
    snapshot: &PreparedTaskSnapshot,
    outcome: &str,
    task_results: &mut Vec<Value>,
    ci_sweep_admission: &mut Vec<Value>,
    drain_approval: &mut Vec<Value>,
) {
    let changed =
        snapshot.context_files != task.after || snapshot.complexity != Some(task.complexity);
    let (field, decision, records) = match task.admission {
        Some(Admission::CiSweep(decision)) => (
            Some("ci_sweep_admission"),
            Some(decision),
            ci_sweep_admission,
        ),
        Some(Admission::Drain(decision)) => {
            (Some("drain_approval"), Some(decision), drain_approval)
        }
        None => (None, None, ci_sweep_admission),
    };
    let approved = decision
        .as_ref()
        .is_some_and(|decision| decision["approved"] == true);
    if let Value::Object(fields) = &mut task.assessment {
        fields.insert(
            "applied".to_string(),
            Value::Bool(changed || task.promote || approved),
        );
        fields.insert("outcome".to_string(), json!(outcome));
        fields.insert("operation_id".to_string(), json!(task.operation_id));
        if let (Some(field), Some(decision)) = (field, decision.clone()) {
            fields.insert(field.to_string(), decision);
        }
    }
    if let Some(decision) = decision {
        records.push(decision);
    }
    task_results.push(task.assessment);
}

pub(in super::super) fn with_task_locks(
    runtime: &OrbitRuntime,
    write_task_id: &str,
    task_ids: &[String],
    index: usize,
    operation: &mut dyn FnMut() -> Result<(), OrbitError>,
) -> Result<(), OrbitError> {
    let Some(task_id) = task_ids.get(index) else {
        return operation();
    };
    let mut nested = || with_task_locks(runtime, write_task_id, task_ids, index + 1, operation);
    if task_id == write_task_id {
        runtime
            .stores()
            .tasks()
            .with_task_write_lock(task_id, &mut nested)
    } else {
        runtime
            .stores()
            .tasks()
            .with_task_read_lock(task_id, &mut nested)
    }
}
