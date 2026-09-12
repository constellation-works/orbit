//! Task-isolated validation, idempotent recovery, and atomic application for task pilots.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use orbit_common::OrbitError;
use orbit_engine::DispatchError;
use orbit_store::contracts::{AtomicTaskMutationOutcome, AtomicTaskMutationParams};
use orbit_types::record::OrbitEvent;
use orbit_types::task::{Task, TaskComplexity, TaskStatus};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::OrbitRuntime;
use crate::adapter::engine_host::v2_host::ci_failure_admission;
#[cfg(test)]
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
    complexity: Option<TaskComplexity>,
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
    complexity: TaskComplexity,
    operation_id: String,
}

const STORAGE_APPLY_ATTEMPTS: usize = 3;

/// Field carrying the deterministic over-attachment finding this boundary
/// injects into an applied assessment [ORB-12228]. It sits beside
/// [`VALIDATION_TOOL_WARNINGS`] so the orchestrator reads the agent's findings
/// and the host's in the same assessment.
const CONTEXT_ATTACHMENT_WARNINGS: &str = "context_attachment_warnings";

/// Selector budget for a proposal at each recommended complexity.
///
/// Context selectors are both the executor's reading list and the task's lock
/// reservations, so a proposal far larger than the assessed repair serializes
/// unrelated work on the same files and hides the real modification targets.
/// Each budget sits well above the largest honest attachment observed for its
/// tier, which makes exceeding it a signal that the pilot swept a directory
/// instead of deriving targets from references. `unassessed` shares the
/// strictest budget: a pilot that could not size the repair has no evidence
/// for reserving a wide surface either.
fn context_selector_cap(complexity: TaskComplexity) -> usize {
    match complexity {
        TaskComplexity::Unassessed | TaskComplexity::Low => 10,
        TaskComplexity::Medium => 20,
        TaskComplexity::Hard => 40,
    }
}

/// Report an over-budget proposal without refusing it. Genuinely large-surface
/// work must stay applyable, so this returns a finding for the orchestrator to
/// weigh rather than a validation error [ORB-12228].
fn over_attachment_findings(complexity: TaskComplexity, after: &[String]) -> Vec<String> {
    let cap = context_selector_cap(complexity);
    if after.len() <= cap {
        return Vec::new();
    }
    vec![format!(
        "over-attached context: {} selectors proposed for {complexity} complexity, above the \
         {cap}-selector budget for that tier; context selectors are lock reservations and the \
         executor's reading list, so each one should be a file this task modifies",
        after.len()
    )]
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
            match apply_task(runtime, snapshot, &validated, prepared_value) {
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
    let repair_claim = (repair_task_ids.len() == prepared_before.len())
        .then(|| prepared.get("state_automation").cloned())
        .flatten()
        .unwrap_or(Value::Null);
    let repair_prepared = json!({
        "state_automation": repair_claim,
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

enum ApplyTaskOutcome {
    Applied(Option<String>),
    AlreadyApplied(Option<String>),
    Stale(&'static str, &'static str),
}

fn apply_task(
    runtime: &OrbitRuntime,
    snapshot: &PreparedTaskSnapshot,
    task: &ValidatedTask,
    prepared: &Value,
) -> Result<ApplyTaskOutcome, OrbitError> {
    if !matches!(snapshot.status, TaskStatus::Proposed | TaskStatus::Backlog) {
        return Ok(ApplyTaskOutcome::Stale(
            "status_not_mutable",
            "task-pilot does not rewrite in-progress, review, or terminal work",
        ));
    }
    let mut lock_ids = vec![task.task_id.clone()];
    lock_ids.extend(runtime.get_task(&task.task_id)?.dependencies());
    lock_ids.sort();
    lock_ids.dedup();
    let mut outcome = None;
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
                snapshot,
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
        if let Some(reason) = task_snapshot_drift(runtime, &current, snapshot) {
            outcome = Some(ApplyTaskOutcome::Stale(reason.0, reason.1));
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
                    snapshot,
                )?));
            }
            AtomicTaskMutationOutcome::AlreadyApplied => {
                outcome = Some(ApplyTaskOutcome::AlreadyApplied(resulting_fingerprint(
                    runtime,
                    &task.task_id,
                    snapshot,
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
    outcome.ok_or_else(|| OrbitError::Execution("task-pilot operation did not run".to_string()))
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
) -> Result<Option<String>, OrbitError> {
    let Some((_, revision)) = &snapshot.material else {
        return Ok(None);
    };
    let current = runtime.get_task(task_id)?;
    crate::application::automation::preparation::fingerprint(runtime, &current, revision)
        .map(Some)
        .map_err(orbit_automation::automation_error_to_orbit)
}

fn task_operation_id(prepared: &Value, task_id: &str, assessment: &Value) -> String {
    let identity = json!({
        "task_id": task_id,
        "source": prepared.get("source"),
        "prepared_tasks": prepared.get("tasks"),
        "assessment": assessment,
    });
    let encoded = serde_json::to_vec(&identity).unwrap_or_default();
    format!("{:x}", Sha256::digest(encoded))
}

fn record_applied_assessment(
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

fn stale_task(task_id: &str, reason: &str, detail: &str) -> Value {
    json!({
        "task_id": task_id,
        "outcome": "stale",
        "reason": reason,
        "detail": detail,
    })
}

fn task_outcome(task_id: &str, outcome: &str, error: Option<String>) -> Value {
    json!({ "task_id": task_id, "outcome": outcome, "error": error })
}

#[cfg(test)]
thread_local! {
    static INJECT_CONCURRENT_EDIT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(in super::super) fn inject_concurrent_edit_before_locked_apply() {
    INJECT_CONCURRENT_EDIT.set(true);
}

#[cfg(test)]
fn inject_concurrent_edit(runtime: &OrbitRuntime, task_id: &str) -> Result<(), OrbitError> {
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
fn inject_concurrent_edit(_runtime: &OrbitRuntime, _task_id: &str) -> Result<(), OrbitError> {
    Ok(())
}

fn failed_partition(partition_index: u64, task_ids: &[String], error: String) -> Value {
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
