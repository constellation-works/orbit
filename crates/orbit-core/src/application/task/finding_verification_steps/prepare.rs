//! Select the review findings a verification run judges, draw each one's
//! verifier crew and partition them by crew.

use std::collections::{BTreeMap, BTreeSet};

use orbit_common::OrbitError;
use orbit_engine::DispatchError;
use orbit_types::task::{Task, TaskType};
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::application::task::{
    TaskListFilter, VERIFIABLE_STATUSES, finding_fingerprint, is_review_finding, needs_verification,
};

use crate::application::task::pilot::input::{
    bounded_usize, requested_workspace_root, string_array,
};
use crate::application::task::pilot::source::resolve_source_snapshot;

const DEFAULT_MAX_PARTITION_SIZE: usize = 5;
const HARD_MAX_PARTITION_SIZE: usize = 5;
const DEFAULT_MAX_TASKS: usize = 50;
const HARD_MAX_TASKS: usize = 500;

pub(crate) fn prepare(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
    run_id: Option<&str>,
) -> Result<Value, DispatchError> {
    let failed = |error: OrbitError| DispatchError::action_failed(action, error.to_string());
    let workspace_root = requested_workspace_root(runtime, action, input)?;
    let claim = crate::application::automation::members::claim(runtime, input).map_err(failed)?;
    // Verdicts cite `path:line` at one revision, so there is nothing to
    // verify against outside a Git work tree.
    let source =
        resolve_source_snapshot(runtime, action, input, &workspace_root)?.ok_or_else(|| {
            DispatchError::action_failed(
                action,
                "finding verification pins a Git revision; the workspace is not a Git work tree",
            )
        })?;
    if let Some(claim) = &claim
        && (source.source_revision != claim.member.source.commit
            || input.get("task_ids") != Some(&json!(claim.task_ids())))
    {
        return Err(DispatchError::action_failed(
            action,
            "state-trigger source or task membership changed",
        ));
    }
    let max_partition_size = bounded_usize(
        action,
        input,
        "max_partition_size",
        DEFAULT_MAX_PARTITION_SIZE,
        HARD_MAX_PARTITION_SIZE,
    )?;
    let max_tasks = bounded_usize(
        action,
        input,
        "max_tasks",
        DEFAULT_MAX_TASKS,
        HARD_MAX_TASKS,
    )?;
    let explicit_task_ids = string_array(input, "task_ids", action)?;
    let explicit = !explicit_task_ids.is_empty();
    if explicit_task_ids.len() > max_tasks {
        return Err(DispatchError::action_failed(
            action,
            format!(
                "explicit selection names {} tasks, exceeding max_tasks {max_tasks}",
                explicit_task_ids.len()
            ),
        ));
    }

    // A retried prepare is not held by its own earlier checkpoint.
    let mut held =
        crate::application::automation::preparation::active_finding_verifications(runtime)
            .map_err(failed)?;
    if let Some(run_id) = run_id {
        held.retain(|_, holders| {
            holders.remove(run_id);
            !holders.is_empty()
        });
    }

    let candidates = if explicit {
        let mut seen = BTreeSet::new();
        explicit_task_ids
            .iter()
            .map(|task_id| {
                if !seen.insert(task_id.as_str()) {
                    return Err(DispatchError::action_failed(
                        action,
                        format!("explicit task_ids contains duplicate {task_id}"),
                    ));
                }
                runtime.get_task(task_id).map_err(|error| {
                    if matches!(error, OrbitError::NotFound { .. }) {
                        DispatchError::action_failed(
                            action,
                            format!("task {task_id} does not exist in the selected workspace"),
                        )
                    } else {
                        failed(error)
                    }
                })
            })
            .collect::<Result<Vec<_>, _>>()?
    } else {
        let mut envelopes = runtime
            .task_candidates(
                &TaskListFilter {
                    statuses: Some(VERIFIABLE_STATUSES.to_vec()),
                    task_type: Some(TaskType::Bug),
                    ..Default::default()
                },
                usize::MAX,
            )
            .map_err(failed)?
            .items;
        // Oldest findings first, so a capped pass makes progress through them.
        envelopes.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then(left.id.cmp(&right.id))
        });
        envelopes
            .iter()
            .map(|envelope| runtime.get_task(&envelope.id))
            .collect::<Result<Vec<_>, _>>()
            .map_err(failed)?
            .into_iter()
            .filter(is_review_finding)
            .collect()
    };

    // An explicitly named task that cannot be verified settles superseded with
    // its reason, so a claimed member is released rather than failed.
    let mut superseded = Vec::new();
    let mut excluded = BTreeMap::<&'static str, usize>::new();
    let mut selected = Vec::new();
    for task in candidates {
        let history = runtime.get_task_history(&task.id).map_err(failed)?;
        let reason = if !is_review_finding(&task) {
            Some("not_a_review_finding")
        } else if !VERIFIABLE_STATUSES.contains(&task.status) {
            Some("status_not_verifiable")
        } else if held.contains_key(&task.id) {
            Some("already_verifying")
        } else if !explicit && !needs_verification(&task, &history) {
            // An explicit selection re-verifies a confirmed finding on request.
            Some("already_confirmed")
        } else {
            None
        };
        match reason {
            Some(reason) if explicit => superseded.push(json!({
                "task_id": task.id, "outcome": "superseded", "reason": reason,
                "status": task.status,
                "detail": skipped_detail(reason, held.get(&task.id)),
            })),
            Some(reason) => *excluded.entry(reason).or_default() += 1,
            None if selected.len() < max_tasks => selected.push((task, history.len())),
            None => *excluded.entry("deferred_by_max_tasks").or_default() += 1,
        }
    }

    // Every task's verifier is drawn before any is dispatched, so its tier
    // and crew are part of the prepared checkpoint apply trusts.
    let mut tasks = Vec::with_capacity(selected.len());
    let mut by_crew = BTreeMap::<String, Vec<String>>::new();
    for (task, history_len) in &selected {
        let verifier = runtime.first_finding_verifier(task).map_err(failed)?;
        by_crew
            .entry(verifier.crew.clone())
            .or_default()
            .push(task.id.clone());
        tasks.push(task_snapshot(task, *history_len, &verifier, &[]));
    }
    let partitions = by_crew
        .iter()
        .flat_map(|(crew, ids)| {
            ids.chunks(max_partition_size)
                .map(move |chunk| (crew.clone(), chunk.to_vec()))
        })
        .enumerate()
        .map(|(partition_index, (crew, task_ids))| {
            json!({"partition_index": partition_index, "task_ids": task_ids, "crew": crew})
        })
        .collect::<Vec<_>>();
    let task_ids = tasks
        .iter()
        .map(|task| task["task_id"].clone())
        .collect::<Vec<_>>();

    Ok(json!({
        "state_automation": claim,
        "mode": if explicit { "explicit" } else { "automatic" },
        "workspace_path": workspace_root,
        "owner_machine": runtime.automation_execution_location(),
        "source": source.to_json(),
        "round": "initial",
        "task_count": task_ids.len(),
        "task_ids": task_ids,
        "tasks": tasks,
        "partition_size": max_partition_size,
        "partition_count": partitions.len(),
        "partitions": partitions,
        "excluded_by_reason": excluded,
        "superseded_during_preparation": superseded,
    }))
}

/// The prepared record of one task apply validates its verdict against.
pub(super) fn task_snapshot(
    task: &Task,
    history_len: usize,
    verifier: &crate::application::task::VerifierAssignment,
    prior: &[Value],
) -> Value {
    json!({
        "task_id": task.id,
        "title": task.title,
        "status": task.status,
        "history_len": history_len,
        "finding_fingerprint": finding_fingerprint(task),
        "crew": verifier.crew,
        "tier": verifier.tier,
        "prior": prior,
    })
}

fn skipped_detail(reason: &str, holders: Option<&BTreeSet<String>>) -> String {
    match reason {
        "not_a_review_finding" => {
            "only a bug task filed by a review is verified; this task is not one".into()
        }
        "status_not_verifiable" => "only a proposed, someday or backlog finding is verified".into(),
        "already_verifying" => format!(
            "verified by run {}",
            holders
                .into_iter()
                .flatten()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ),
        _ => reason.into(),
    }
}
