//! Task-isolated validation, idempotent recovery, and atomic application for task pilots.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use orbit_common::OrbitError;
use orbit_engine::DispatchError;
use orbit_types::task::{TaskComplexity, TaskStatus};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::adapter::engine_host::v2_host::ci_failure::admission as ci_failure_admission;

use super::attachment_budget::{
    CONTEXT_ATTACHMENT_WARNINGS, over_attachment_findings, resolve_applied_complexity,
};
use super::persist::{
    ApplyTaskOutcome, apply_task, failed_partition, inject_concurrent_edit,
    record_applied_assessment, stale_task, task_operation_id, task_outcome,
};
use super::source::SourceSnapshot;
use super::{
    VALIDATION_TOOL_WARNINGS, action_failed, member_ready, requested_workspace_root,
    required_string, required_string_array, string_array, string_array_value,
    validate_after_selectors, validate_recommendations,
};

#[derive(Clone)]
pub(super) struct PreparedTaskSnapshot {
    pub(super) context_files: Vec<String>,
    pub(super) status: TaskStatus,
    pub(super) complexity: Option<TaskComplexity>,
    pub(super) title: String,
    pub(super) tags: Vec<String>,
    pub(super) material: Option<(String, String)>,
    pub(super) status_neutral_fingerprint: Option<String>,
    /// Deterministic feasibility findings for the tools this task's acceptance
    /// criteria require, computed at preparation [ORB-11980].
    validation_tool_warnings: Vec<String>,
}

pub(super) struct ValidatedTask {
    pub(super) task_id: String,
    pub(super) after: Vec<String>,
    pub(super) assessment: Value,
    pub(super) admission: Option<Value>,
    pub(super) promote: bool,
    pub(super) complexity: TaskComplexity,
    pub(super) operation_id: String,
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
    let prior_applied_count = input
        .get("prior_applied_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let carried_task_outcomes = input
        .get("carried_task_outcomes")
        .map(|value| {
            value
                .as_array()
                .cloned()
                .ok_or_else(|| action_failed(action, "carried_task_outcomes must be an array"))
        })
        .transpose()?
        .unwrap_or_default();
    let claim = crate::application::automation::members::claim(runtime, prepared_value, &[])
        .map_err(|error| action_failed(action, error.to_string()))?;
    // The same consumer predicate prepare fingerprinted under; a run without
    // a claim evaluates the default [ORB-12745].
    let eligibility =
        crate::application::automation::preparation::claim_eligibility(runtime, claim.as_ref())
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
                    complexity: serde_json::from_value::<Option<TaskComplexity>>(
                        entry.get("complexity").cloned().unwrap_or(Value::Null),
                    )
                    .map_err(|error| {
                        action_failed(
                            action,
                            format!("prepared task complexity is invalid: {error}"),
                        )
                    })?,
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
                    status_neutral_fingerprint: entry
                        .get("status_neutral_fingerprint")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned),
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
        let assessment_ids = assessments_by_id.keys().cloned().collect::<BTreeSet<_>>();
        let expected_id_set = expected_ids.iter().cloned().collect::<BTreeSet<_>>();
        if assessment_ids != expected_id_set {
            partition_decisions.push(failed_partition(
                expected_index,
                &expected_ids,
                format!(
                    "partition {expected_index} task assessment identities do not match the prepared partition"
                ),
            ));
            continue;
        }

        let mut outcomes = Vec::with_capacity(expected_ids.len());
        let mut applied_task_ids = Vec::new();
        for task_id in &expected_ids {
            let Some(assessment) = assessments_by_id.get(task_id) else {
                outcomes.push(task_outcome(
                    task_id,
                    "invalid",
                    Some(format!("partition {expected_index} omitted task {task_id}")),
                ));
                continue;
            };
            let snapshot = &prepared_before[task_id];
            let reported_before =
                match required_string_array(assessment, "context_files_before", action) {
                    Ok(before) => before,
                    Err(error) => {
                        outcomes.push(task_outcome(task_id, "invalid", Some(error.to_string())));
                        continue;
                    }
                };
            if reported_before != snapshot.context_files {
                outcomes.push(stale_task(
                    task_id,
                    "reported_context_snapshot_mismatch",
                    "agent context_files_before does not match this run's prepared snapshot",
                ));
                continue;
            }
            let disposition = match required_string(assessment, "disposition", action) {
                Ok(value) => value,
                Err(error) => {
                    outcomes.push(task_outcome(task_id, "invalid", Some(error.to_string())));
                    continue;
                }
            };
            let proposed_after =
                match required_string_array(assessment, "context_files_after", action) {
                    Ok(value) => value,
                    Err(error) => {
                        outcomes.push(task_outcome(task_id, "invalid", Some(error.to_string())));
                        continue;
                    }
                };
            let selectors = match validate_after_selectors(
                action,
                task_id,
                disposition,
                assessment,
                &proposed_after,
                &workspace_root,
                source.as_ref(),
            ) {
                Ok(selectors) => selectors,
                Err(error) => {
                    outcomes.push(task_outcome(task_id, "invalid", Some(error.to_string())));
                    continue;
                }
            };
            let after = selectors.values;
            let complexity = match validate_recommendations(action, task_id, assessment) {
                Ok(complexity) => complexity,
                Err(error) => {
                    outcomes.push(task_outcome(task_id, "invalid", Some(error.to_string())));
                    continue;
                }
            };
            let complexity = resolve_applied_complexity(complexity, snapshot.complexity);
            let current = match runtime.get_task(task_id) {
                Ok(task) => task,
                Err(OrbitError::NotFound { .. }) => {
                    outcomes.push(stale_task(
                        task_id,
                        "task_deleted",
                        "task no longer exists after preparation",
                    ));
                    continue;
                }
                Err(error) => {
                    outcomes.push(task_outcome(
                        task_id,
                        "apply_failed",
                        Some(format!("reload task {task_id}: {error}")),
                    ));
                    continue;
                }
            };
            // The pilot never sees the deterministic findings — the lane's
            // validation-tool feasibility [ORB-11980] and this boundary's
            // over-attachment budget [ORB-12228] — so apply attaches them
            // here: every downstream readiness and admission rule then reads
            // one assessment carrying both the agent's findings and the
            // host's.
            let mut assessment = (*assessment).clone();
            if let Value::Object(fields) = &mut assessment {
                fields.insert(
                    VALIDATION_TOOL_WARNINGS.to_string(),
                    json!(snapshot.validation_tool_warnings),
                );
                fields.insert("context_files_after".to_string(), json!(after));
                fields.insert(
                    "selector_normalizations".to_string(),
                    json!(selectors.normalizations),
                );
                fields.insert(
                    CONTEXT_ATTACHMENT_WARNINGS.to_string(),
                    json!(over_attachment_findings(complexity, &after)),
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
                    outcomes.push(task_outcome(task_id, "invalid", Some(error.to_string())));
                    continue;
                }
            };
            let promote = admission
                .as_ref()
                .is_some_and(|decision| decision["decision"] == "promote");
            let operation_id = task_operation_id(prepared_value, task_id, &assessment);
            let validated = ValidatedTask {
                task_id: task_id.clone(),
                after,
                assessment,
                admission,
                promote,
                complexity,
                operation_id,
            };

            inject_concurrent_edit(runtime, task_id)
                .map_err(|error| action_failed(action, error.to_string()))?;
            match apply_task(runtime, snapshot, &validated, prepared_value, &eligibility) {
                Ok(ApplyTaskOutcome::Applied(fingerprint)) => {
                    if let Some(fingerprint) = fingerprint {
                        resulting_fingerprints.insert(task_id.clone(), fingerprint);
                    }
                    applied_task_ids.push(task_id.clone());
                    outcomes.push(task_outcome(task_id, "applied", None));
                    record_applied_assessment(
                        validated,
                        snapshot,
                        "applied",
                        &mut task_results,
                        &mut ci_sweep_admission,
                    );
                }
                Ok(ApplyTaskOutcome::AlreadyApplied(fingerprint)) => {
                    if let Some(fingerprint) = fingerprint {
                        resulting_fingerprints.insert(task_id.clone(), fingerprint);
                    }
                    applied_task_ids.push(task_id.clone());
                    outcomes.push(task_outcome(task_id, "already_applied", None));
                    record_applied_assessment(
                        validated,
                        snapshot,
                        "already_applied",
                        &mut task_results,
                        &mut ci_sweep_admission,
                    );
                }
                Ok(ApplyTaskOutcome::Stale(reason, detail)) => {
                    outcomes.push(stale_task(task_id, reason, detail));
                }
                Err(error) => outcomes.push(task_outcome(
                    task_id,
                    "apply_failed",
                    Some(error.to_string()),
                )),
            }
        }

        let unresolved = outcomes
            .iter()
            .filter(|outcome| {
                !matches!(
                    outcome["outcome"].as_str(),
                    Some("applied" | "already_applied")
                )
            })
            .count();
        let outcome = if unresolved == 0 {
            "applied"
        } else if applied_task_ids.is_empty()
            && outcomes.iter().all(|outcome| outcome["outcome"] == "stale")
        {
            "skipped_stale"
        } else if applied_task_ids.is_empty() {
            "failed"
        } else {
            "partial"
        };
        let error = outcomes.iter().find_map(|task| {
            (!matches!(
                task["outcome"].as_str(),
                Some("applied" | "already_applied")
            ))
            .then(|| {
                task.get("error")
                    .and_then(Value::as_str)
                    .or_else(|| task.get("detail").and_then(Value::as_str))
                    .map(ToOwned::to_owned)
            })
            .flatten()
        });
        let stale_tasks = outcomes
            .iter()
            .filter(|task| task["outcome"] == "stale")
            .cloned()
            .collect::<Vec<_>>();
        partition_decisions.push(json!({
            "partition_index": expected_index,
            "task_ids": expected_ids,
            "outcome": outcome,
            "applied_task_ids": applied_task_ids,
            "task_outcomes": outcomes,
            "unresolved_count": unresolved,
            "error": error,
            "stale_tasks": stale_tasks,
        }));
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
            "unresolved_count": 1,
            "applied_task_ids": [],
            "task_outcomes": [],
        }));
    }

    let failed_partitions = partition_decisions
        .iter()
        .filter(|decision| matches!(decision["outcome"].as_str(), Some("failed" | "partial")))
        .cloned()
        .collect::<Vec<_>>();
    let own_task_outcomes = partition_decisions
        .iter()
        .filter_map(|decision| decision["task_outcomes"].as_array())
        .flatten()
        .cloned()
        .collect::<Vec<_>>();
    let mut task_outcomes = carried_task_outcomes.clone();
    task_outcomes.extend(own_task_outcomes.iter().cloned());
    let skipped_stale_partitions = partition_decisions
        .iter()
        .filter(|decision| decision["outcome"] == "skipped_stale")
        .cloned()
        .collect::<Vec<_>>();
    let applied_tasks = prior_applied_count as usize
        + partition_decisions
            .iter()
            .filter_map(|decision| decision["applied_task_ids"].as_array())
            .map(Vec::len)
            .sum::<usize>();
    let unresolved_tasks = carried_task_outcomes.len() as u64
        + partition_decisions
            .iter()
            .filter_map(|decision| decision.get("unresolved_count").and_then(Value::as_u64))
            .sum::<u64>();
    let succeeded = failed_partitions.is_empty()
        && skipped_stale_partitions.is_empty()
        && carried_task_outcomes.is_empty();
    let status = if succeeded { "succeeded" } else { "failed" };
    let error = (!succeeded).then(|| {
        let first_unresolved = partition_decisions
            .iter()
            .find_map(|partition| {
                let partition_index = partition["partition_index"].as_u64()?;
                let task = partition["task_outcomes"]
                    .as_array()?
                    .iter()
                    .find(|task| {
                        !matches!(task["outcome"].as_str(), Some("applied" | "already_applied"))
                    })?;
                let classification = task
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| task["outcome"].as_str().unwrap_or("unresolved"));
                let detail = task
                    .get("error")
                    .and_then(Value::as_str)
                    .or_else(|| task.get("detail").and_then(Value::as_str))
                    .unwrap_or("unresolved task");
                Some(format!(
                    "partition {partition_index}, task {}: {classification}: {detail}",
                    task["task_id"].as_str().unwrap_or("<unknown>"),
                ))
            })
            .or_else(|| {
                carried_task_outcomes.first().map(|task| {
                    format!(
                        "carried task {}: {}",
                        task["task_id"].as_str().unwrap_or("<unknown>"),
                        task.get("error")
                            .and_then(Value::as_str)
                            .or_else(|| task.get("detail").and_then(Value::as_str))
                            .unwrap_or("unresolved task")
                    )
                })
            })
            .unwrap_or_else(|| "partition envelope validation failed".to_string());
        format!(
            "task-pilot apply unresolved: {first_unresolved}; {applied_tasks} applied, {unresolved_tasks} unresolved"
        )
    });
    let repair_task_ids = own_task_outcomes
        .iter()
        .filter(|outcome| outcome["outcome"] == "invalid")
        .filter_map(|outcome| outcome["task_id"].as_str().map(ToOwned::to_owned))
        .collect::<Vec<_>>();
    let repair_partitions = repair_task_ids
        .iter()
        .enumerate()
        .map(|(partition_index, task_id)| {
            let errors = own_task_outcomes
                .iter()
                .filter(|outcome| outcome["task_id"].as_str() == Some(task_id))
                .filter_map(|outcome| outcome["error"].as_str())
                .collect::<Vec<_>>();
            json!({
                "partition_index": partition_index,
                "task_ids": [task_id],
                "validation_errors": errors,
                "source_revision": source.as_ref().map(|source| source.source_revision.as_str()),
            })
        })
        .collect::<Vec<_>>();
    let repair_tasks = prepared_tasks
        .iter()
        .filter(|task| {
            task["task_id"]
                .as_str()
                .is_some_and(|id| repair_task_ids.iter().any(|repair_id| repair_id == id))
        })
        .cloned()
        .collect::<Vec<_>>();
    // The repair apply keeps the claim so a repaired member still certifies
    // its own evidence under the consumer's predicate [ORB-12746].
    let repair_prepared = json!({
        "state_automation": prepared.get("state_automation").cloned().unwrap_or(Value::Null),
        "mode": mode,
        "workspace_path": workspace_root,
        "source": prepared.get("source").cloned().unwrap_or(Value::Null),
        "task_count": repair_task_ids.len(),
        "task_ids": repair_task_ids,
        "tasks": repair_tasks,
        "partition_size": 1,
        "partition_count": repair_partitions.len(),
        "partitions": repair_partitions,
        "excluded": [],
    });
    let non_repairable_outcomes = task_outcomes
        .iter()
        .filter(|outcome| {
            !matches!(
                outcome["outcome"].as_str(),
                Some("invalid" | "applied" | "already_applied")
            )
        })
        .cloned()
        .collect::<Vec<_>>();

    // One evidence record per claim member this apply settled, independent
    // of its siblings: a failed partition never withholds an applied member's
    // receipt [ORB-12746].
    let member_evidence = claim.as_ref().map(|claim| {
        claim
            .members()
            .iter()
            .filter_map(|member| {
                let id = member.task_ids.first()?;
                let resulting = resulting_fingerprints.get(id)?;
                let assessment = task_results
                    .iter()
                    .find(|v| v["task_id"].as_str() == Some(id))?;
                Some(orbit_types::workflow::automation::members::MemberEvidence {
                    action_id: claim.action_id.clone().unwrap_or_default(),
                    attempt_id: claim.id.clone(),
                    member_key: member.key.clone(),
                    input_fingerprint: member.fingerprint.clone(),
                    resulting_fingerprint: resulting.clone(),
                    ready: member_ready(assessment),
                    result: assessment.clone(),
                })
            })
            .collect::<Vec<_>>()
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
        "task_outcomes": task_outcomes,
        "applied_count": applied_tasks,
        "unresolved_count": unresolved_tasks,
        "repair_count": repair_task_ids.len(),
        "repair_partitions": repair_partitions,
        "repair_prepared": repair_prepared,
        "non_repairable_outcomes": non_repairable_outcomes,
        "tasks": task_results,
        "ci_sweep_admission": ci_sweep_admission,
    }))
}
