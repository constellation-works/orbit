//! Partition-isolated validation and compare-and-set application for task pilots.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use orbit_common::OrbitError;
use orbit_engine::DispatchError;
use orbit_types::task::{Task, TaskStatus};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::adapter::engine_host::v2_host::ci_failure_admission;
use crate::application::task::TaskUpdateParams;

use super::source::SourceSnapshot;
use super::{
    VALIDATION_TOOL_WARNINGS, action_failed, member_ready, requested_workspace_root,
    required_string, required_string_array, string_array, string_array_value,
    validate_after_selectors, validate_recommendations,
};

#[derive(Clone)]
struct PreparedTaskSnapshot {
    context_files: Vec<String>,
    status: TaskStatus,
    title: String,
    tags: Vec<String>,
    material: Option<(String, String)>,
    /// Deterministic feasibility findings for the tools this task's acceptance
    /// criteria require, computed at preparation [ORB-11980].
    validation_tool_warnings: Vec<String>,
}

struct ValidatedTask {
    task_id: String,
    after: Vec<String>,
    assessment: Value,
    admission: Option<Value>,
    promote: bool,
}

pub(in super::super) fn apply(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
) -> Result<Value, DispatchError> {
    let workspace_root = requested_workspace_root(runtime, action, input)?;
    let prepared_value = input
        .get("prepared")
        .ok_or_else(|| action_failed(action, "`prepared` must be an object"))?;
    let prepared = prepared_value
        .as_object()
        .ok_or_else(|| action_failed(action, "`prepared` must be an object"))?;
    let prepared_workspace = prepared
        .get("workspace_path")
        .and_then(Value::as_str)
        .ok_or_else(|| action_failed(action, "prepared.workspace_path must be a string"))?;
    if Path::new(prepared_workspace) != workspace_root {
        return Err(action_failed(
            action,
            format!(
                "prepared workspace {prepared_workspace} does not match active workspace {}",
                workspace_root.display()
            ),
        ));
    }
    let mode = prepared
        .get("mode")
        .and_then(Value::as_str)
        .ok_or_else(|| action_failed(action, "prepared.mode must be a string"))?;
    let expected_partitions = prepared
        .get("partitions")
        .and_then(Value::as_array)
        .ok_or_else(|| action_failed(action, "prepared.partitions must be an array"))?;
    let prepared_tasks = prepared
        .get("tasks")
        .and_then(Value::as_array)
        .ok_or_else(|| action_failed(action, "prepared.tasks must be an array"))?;
    let results = input
        .get("results")
        .and_then(Value::as_array)
        .ok_or_else(|| action_failed(action, "`results` must be an array"))?;
    let claim = crate::application::automation::members::claim(runtime, prepared_value)
        .map_err(|error| action_failed(action, error.to_string()))?;
    let source = SourceSnapshot::from_prepared(prepared_value, action)?;
    if let Some(source) = &source {
        source.ensure_commit(action, &workspace_root)?;
    }

    let prepared_before = prepared_tasks
        .iter()
        .map(|entry| {
            let task_id = required_string(entry, "task_id", action)?;
            let context_files = required_string_array(entry, "context_files_before", action)?;
            let status = serde_json::from_value::<TaskStatus>(
                entry
                    .get("status")
                    .cloned()
                    .ok_or_else(|| action_failed(action, "prepared task is missing status"))?,
            )
            .map_err(|error| {
                action_failed(action, format!("prepared task status is invalid: {error}"))
            })?;
            let title = required_string(entry, "title", action)?.to_string();
            let tags = required_string_array(entry, "tags", action)?;
            Ok((
                task_id.to_string(),
                PreparedTaskSnapshot {
                    context_files,
                    status,
                    title,
                    tags,
                    validation_tool_warnings: string_array(
                        entry,
                        VALIDATION_TOOL_WARNINGS,
                        action,
                    )?,
                    material: entry
                        .get("material_fingerprint")
                        .and_then(Value::as_str)
                        .zip(source.as_ref().map(|s| s.source_revision.as_str()))
                        .map(|(fingerprint, revision)| {
                            (fingerprint.to_string(), revision.to_string())
                        }),
                },
            ))
        })
        .collect::<Result<BTreeMap<_, _>, DispatchError>>()?;
    if prepared_before.len() != prepared_tasks.len() {
        return Err(action_failed(
            action,
            "prepared.tasks contains duplicate task snapshots",
        ));
    }

    // Each partition is its own validation boundary. A malformed or stale
    // partition mutates none of its tasks, but it cannot discard an unrelated
    // partition whose agent result and prepared snapshot are still current.
    let ci_sweep_filing = input
        .get("ci_sweep_filing")
        .filter(|value| !value.is_null());
    let promotion_authorized = input
        .get("promotion_authorized")
        .map(|value| {
            value
                .as_bool()
                .ok_or_else(|| action_failed(action, "promotion_authorized must be a boolean"))
        })
        .transpose()?
        .unwrap_or(false);
    if ci_sweep_filing.is_some() && prepared_before.len() != 1 {
        return Err(action_failed(
            action,
            "CI-sweep admission requires exactly one prepared task",
        ));
    }

    let mut seen_task_ids = BTreeSet::new();
    let mut partition_decisions = Vec::with_capacity(expected_partitions.len());
    let mut task_results = Vec::with_capacity(prepared_before.len());
    let mut ci_sweep_admission = Vec::new();
    let mut resulting_fingerprints = BTreeMap::new();

    for (position, expected) in expected_partitions.iter().enumerate() {
        let expected_index = expected
            .get("partition_index")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                action_failed(
                    action,
                    format!("prepared.partitions[{position}].partition_index is invalid"),
                )
            })?;
        let expected_ids = required_string_array(expected, "task_ids", action)?;
        for task_id in &expected_ids {
            if !seen_task_ids.insert(task_id.clone()) {
                return Err(action_failed(
                    action,
                    format!("task {task_id} appears in more than one prepared partition"),
                ));
            }
            if !prepared_before.contains_key(task_id) {
                return Err(action_failed(
                    action,
                    format!("task {task_id} was not in prepared.tasks"),
                ));
            }
        }

        let Some(result) = results.get(position) else {
            partition_decisions.push(failed_partition(
                expected_index,
                &expected_ids,
                format!("partition result {position} is missing"),
            ));
            continue;
        };
        let Some(result_object) = result.as_object() else {
            partition_decisions.push(failed_partition(
                expected_index,
                &expected_ids,
                format!("partition {expected_index} failed or returned no structured result"),
            ));
            continue;
        };
        let result_index = result_object
            .get("partition_index")
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("partition result {position} is missing partition_index"));
        let result_index = match result_index {
            Ok(index) => index,
            Err(error) => {
                partition_decisions.push(failed_partition(expected_index, &expected_ids, error));
                continue;
            }
        };
        if result_index != expected_index {
            partition_decisions.push(failed_partition(
                expected_index,
                &expected_ids,
                format!(
                    "partition result {position} reports index {result_index}, expected {expected_index}"
                ),
            ));
            continue;
        }
        let result_ids = match result_object
            .get("task_ids")
            .ok_or_else(|| action_failed(action, "`task_ids` must be an array"))
            .and_then(|value| string_array_value(value, "task_ids", action))
        {
            Ok(ids) => ids,
            Err(error) => {
                partition_decisions.push(failed_partition(
                    expected_index,
                    &expected_ids,
                    error.to_string(),
                ));
                continue;
            }
        };
        if result_ids != expected_ids {
            partition_decisions.push(failed_partition(
                expected_index,
                &expected_ids,
                format!("partition {expected_index} task_ids do not match the prepared partition"),
            ));
            continue;
        }
        let Some(assessments) = result_object.get("tasks").and_then(Value::as_array) else {
            partition_decisions.push(failed_partition(
                expected_index,
                &expected_ids,
                format!("partition {expected_index}.tasks must be an array"),
            ));
            continue;
        };
        if assessments.len() != expected_ids.len() {
            partition_decisions.push(failed_partition(
                expected_index,
                &expected_ids,
                format!(
                    "partition {expected_index} returned {} task assessments for {} tasks",
                    assessments.len(),
                    expected_ids.len()
                ),
            ));
            continue;
        }
        let assessments_by_id = match assessments
            .iter()
            .map(|assessment| {
                let task_id = required_string(assessment, "task_id", action)?;
                Ok((task_id.to_string(), assessment))
            })
            .collect::<Result<BTreeMap<_, _>, DispatchError>>()
        {
            Ok(by_id) => by_id,
            Err(error) => {
                partition_decisions.push(failed_partition(
                    expected_index,
                    &expected_ids,
                    error.to_string(),
                ));
                continue;
            }
        };
        if assessments_by_id.len() != assessments.len() {
            partition_decisions.push(failed_partition(
                expected_index,
                &expected_ids,
                format!("partition {expected_index} contains duplicate task assessments"),
            ));
            continue;
        }

        let mut validated = Vec::with_capacity(expected_ids.len());
        let mut stale = Vec::new();
        let mut validation_error = None;
        for task_id in &expected_ids {
            let Some(assessment) = assessments_by_id.get(task_id) else {
                validation_error =
                    Some(format!("partition {expected_index} omitted task {task_id}"));
                break;
            };
            let snapshot = &prepared_before[task_id];
            let reported_before =
                match required_string_array(assessment, "context_files_before", action) {
                    Ok(before) => before,
                    Err(error) => {
                        validation_error = Some(error.to_string());
                        break;
                    }
                };
            if reported_before != snapshot.context_files {
                stale.push(stale_task(
                    task_id,
                    "reported_context_snapshot_mismatch",
                    "agent context_files_before does not match this run's prepared snapshot",
                ));
                continue;
            }
            let disposition = match required_string(assessment, "disposition", action) {
                Ok(value) => value,
                Err(error) => {
                    validation_error = Some(error.to_string());
                    break;
                }
            };
            let after = match required_string_array(assessment, "context_files_after", action) {
                Ok(value) => value,
                Err(error) => {
                    validation_error = Some(error.to_string());
                    break;
                }
            };
            if let Err(error) = validate_after_selectors(
                action,
                task_id,
                disposition,
                assessment,
                &after,
                &workspace_root,
                source.as_ref(),
            ) {
                validation_error = Some(error.to_string());
                break;
            }
            if let Err(error) = validate_recommendations(action, task_id, assessment) {
                validation_error = Some(error.to_string());
                break;
            }
            let current = match runtime.get_task(task_id) {
                Ok(task) => task,
                Err(OrbitError::NotFound { .. }) => {
                    stale.push(stale_task(
                        task_id,
                        "task_deleted",
                        "task no longer exists after preparation",
                    ));
                    continue;
                }
                Err(error) => {
                    validation_error = Some(format!("reload task {task_id}: {error}"));
                    break;
                }
            };
            if let Some(reason) = task_snapshot_drift(runtime, &current, snapshot) {
                stale.push(stale_task(task_id, reason.0, reason.1));
                continue;
            }
            // The pilot never sees the deterministic feasibility findings, so
            // apply attaches them here: every downstream readiness and
            // admission rule then reads one assessment that carries both the
            // agent's findings and the lane's [ORB-11980].
            let mut assessment = (*assessment).clone();
            if let Value::Object(fields) = &mut assessment {
                fields.insert(
                    VALIDATION_TOOL_WARNINGS.to_string(),
                    json!(snapshot.validation_tool_warnings),
                );
            }

            let admission = match ci_sweep_filing
                .map(|filing| {
                    ci_failure_admission::assess(
                        action,
                        task_id,
                        &current,
                        &assessment,
                        &after,
                        filing,
                        promotion_authorized,
                    )
                })
                .transpose()
            {
                Ok(admission) => admission,
                Err(error) => {
                    validation_error = Some(error.to_string());
                    break;
                }
            };
            let promote = admission
                .as_ref()
                .is_some_and(|decision| decision["decision"] == "promote");
            validated.push(ValidatedTask {
                task_id: task_id.clone(),
                after,
                assessment,
                admission,
                promote,
            });
        }
        if let Some(error) = validation_error {
            partition_decisions.push(failed_partition(expected_index, &expected_ids, error));
            continue;
        }
        if !stale.is_empty() {
            partition_decisions.push(stale_partition(expected_index, &expected_ids, stale));
            continue;
        }

        match apply_partition(runtime, &prepared_before, &validated, prepared_value) {
            Ok(ApplyPartitionOutcome::Applied(fingerprints)) => {
                resulting_fingerprints.extend(fingerprints);
                let mut applied_task_ids = Vec::with_capacity(validated.len());
                for mut task in validated {
                    let changed = prepared_before[&task.task_id].context_files != task.after;
                    if let Value::Object(fields) = &mut task.assessment {
                        fields.insert("applied".to_string(), Value::Bool(changed || task.promote));
                        if let Some(admission) = task.admission.clone() {
                            fields.insert("ci_sweep_admission".to_string(), admission);
                        }
                    }
                    if let Some(admission) = task.admission {
                        ci_sweep_admission.push(admission);
                    }
                    applied_task_ids.push(task.task_id);
                    task_results.push(task.assessment);
                }
                partition_decisions.push(json!({
                    "partition_index": expected_index,
                    "task_ids": expected_ids,
                    "outcome": "applied",
                    "applied_task_ids": applied_task_ids,
                }));
            }
            Ok(ApplyPartitionOutcome::Stale(stale)) => {
                partition_decisions.push(stale_partition(expected_index, &expected_ids, stale));
            }
            Err(error) => {
                partition_decisions.push(failed_partition(
                    expected_index,
                    &expected_ids,
                    error.to_string(),
                ));
            }
        }
    }

    if seen_task_ids.len() != prepared_before.len() {
        return Err(action_failed(
            action,
            "prepared partitions did not cover every prepared task",
        ));
    }
    for position in expected_partitions.len()..results.len() {
        partition_decisions.push(json!({
            "partition_index": Value::Null,
            "task_ids": [],
            "outcome": "failed",
            "error": format!("unexpected extra partition result at position {position}"),
        }));
    }

    let failed_partitions = partition_decisions
        .iter()
        .filter(|decision| decision["outcome"] == "failed")
        .cloned()
        .collect::<Vec<_>>();
    let skipped_stale_partitions = partition_decisions
        .iter()
        .filter(|decision| decision["outcome"] == "skipped_stale")
        .cloned()
        .collect::<Vec<_>>();
    let applied_partitions = partition_decisions
        .iter()
        .filter(|decision| decision["outcome"] == "applied")
        .count();
    let succeeded = failed_partitions.is_empty() && skipped_stale_partitions.is_empty();
    let status = if succeeded { "succeeded" } else { "failed" };
    let error = (!succeeded).then(|| {
        let stale_tasks = skipped_stale_partitions
            .iter()
            .flat_map(|partition| partition["stale_tasks"].as_array())
            .flatten()
            .filter_map(|task| {
                Some(format!(
                    "{}: {}",
                    task["task_id"].as_str()?,
                    task["reason"].as_str()?
                ))
            })
            .collect::<Vec<_>>();
        let stale_details = if stale_tasks.is_empty() {
            String::new()
        } else {
            format!(" ({})", stale_tasks.join(", "))
        };
        let applied_details = if applied_partitions == 0 {
            String::new()
        } else {
            "; valid partitions were applied".to_string()
        };
        format!(
            "{} partition(s) failed, {} partition(s) were skipped as stale{}, and {} partition(s) applied{}",
            failed_partitions.len(),
            skipped_stale_partitions.len(),
            stale_details,
            applied_partitions,
            applied_details
        )
    });

    let member_evidence = claim.filter(|_| succeeded).and_then(|claim| {
        let id = claim.member.task_ids.first()?;
        let resulting = resulting_fingerprints.get(id)?;
        let assessment = task_results
            .iter()
            .find(|v| v["task_id"].as_str() == Some(id))?;
        Some(orbit_types::workflow::automation::members::MemberEvidence {
            action_id: claim.action_id.unwrap_or_default(),
            attempt_id: claim.id,
            member_key: claim.member.key,
            input_fingerprint: claim.member.fingerprint,
            resulting_fingerprint: resulting.clone(),
            ready: member_ready(assessment),
            result: assessment.clone(),
        })
    });
    Ok(json!({
        "member_evidence": member_evidence,
        "status": status,
        "error": error,
        "mode": mode,
        "workspace_path": workspace_root,
        "source": prepared.get("source").cloned().unwrap_or(Value::Null),
        "discovery": {
            "task_ids": prepared.get("task_ids").cloned().unwrap_or_else(|| json!([])),
            "excluded": prepared.get("excluded").cloned().unwrap_or_else(|| json!([])),
        },
        "partition_decisions": partition_decisions,
        "partition_count": expected_partitions.len(),
        "received_partition_count": results.len(),
        "failed_partitions": failed_partitions,
        "skipped_stale_partitions": skipped_stale_partitions,
        "tasks": task_results,
        "ci_sweep_admission": ci_sweep_admission,
    }))
}

enum ApplyPartitionOutcome {
    Applied(BTreeMap<String, String>),
    Stale(Vec<Value>),
}

fn apply_partition(
    runtime: &OrbitRuntime,
    snapshots: &BTreeMap<String, PreparedTaskSnapshot>,
    tasks: &[ValidatedTask],
    prepared: &Value,
) -> Result<ApplyPartitionOutcome, OrbitError> {
    let mut task_ids = tasks
        .iter()
        .map(|task| task.task_id.clone())
        .collect::<Vec<_>>();
    task_ids.sort();
    let mut lock_ids = task_ids.clone();
    for id in &task_ids {
        lock_ids.extend(runtime.get_task(id)?.dependencies());
    }
    lock_ids.sort();
    lock_ids.dedup();
    let mut outcome = None;
    let mut operation = || {
        crate::application::automation::members::claim(runtime, prepared)?;
        let mut fingerprints = BTreeMap::new();
        let mut stale = Vec::new();
        for task_id in &task_ids {
            match runtime.get_task(task_id) {
                Ok(current) => {
                    if let Some(reason) =
                        task_snapshot_drift(runtime, &current, &snapshots[task_id])
                    {
                        stale.push(stale_task(task_id, reason.0, reason.1));
                    }
                }
                Err(OrbitError::NotFound { .. }) => stale.push(stale_task(
                    task_id,
                    "task_deleted",
                    "task no longer exists at the write boundary",
                )),
                Err(error) => return Err(error),
            }
        }
        if !stale.is_empty() {
            outcome = Some(ApplyPartitionOutcome::Stale(stale));
            return Ok(());
        }

        for task in tasks.iter() {
            let changed = snapshots[&task.task_id].context_files != task.after;
            if changed || task.promote {
                runtime.update_task(
                    &task.task_id,
                    TaskUpdateParams {
                        context_files: changed.then_some(task.after.clone()),
                        status: task.promote.then_some(TaskStatus::Backlog),
                        comment: task.promote.then(|| {
                            "CI-sweep admission: task-pilot validated current relevance and selectors; promoted proposed repair to backlog."
                                .to_string()
                        }),
                        ..TaskUpdateParams::default()
                    },
                )?;
            }
        }
        for task in tasks {
            if let Some((_, revision)) = &snapshots[&task.task_id].material {
                let current = runtime.get_task(&task.task_id)?;
                fingerprints.insert(
                    task.task_id.clone(),
                    crate::application::automation::preparation::fingerprint(
                        runtime, &current, revision,
                    )
                    .map_err(orbit_automation::automation_error_to_orbit)?,
                );
            }
        }
        outcome = Some(ApplyPartitionOutcome::Applied(fingerprints));
        Ok(())
    };
    with_task_locks(runtime, &lock_ids, 0, &mut operation)?;
    outcome.ok_or_else(|| {
        OrbitError::Execution("task-pilot partition operation did not run".to_string())
    })
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
) -> Option<(&'static str, &'static str)> {
    if snapshot
        .material
        .as_ref()
        .is_some_and(|(expected, revision)| {
            crate::application::automation::preparation::fingerprint(runtime, current, revision)
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
    } else if current.title != snapshot.title {
        Some(("title_changed", "task title changed after preparation"))
    } else if current.tags != snapshot.tags {
        Some(("tags_changed", "task tags changed after preparation"))
    } else {
        None
    }
}

fn stale_task(task_id: &str, reason: &str, detail: &str) -> Value {
    json!({ "task_id": task_id, "reason": reason, "detail": detail })
}

fn failed_partition(partition_index: u64, task_ids: &[String], error: String) -> Value {
    json!({
        "partition_index": partition_index,
        "task_ids": task_ids,
        "outcome": "failed",
        "error": error,
    })
}

fn stale_partition(partition_index: u64, task_ids: &[String], stale: Vec<Value>) -> Value {
    json!({
        "partition_index": partition_index,
        "task_ids": task_ids,
        "outcome": "skipped_stale",
        "stale_tasks": stale,
        "applied_task_ids": [],
    })
}
