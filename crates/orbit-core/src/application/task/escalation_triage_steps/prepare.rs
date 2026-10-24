//! Select the held pilot assessments an escalation triage run reads, draw
//! each one's hard-tier crew and partition them by crew.

use std::collections::{BTreeMap, BTreeSet};

use orbit_common::OrbitError;
use orbit_engine::DispatchError;
use orbit_types::task::{Task, TaskComplexity, TaskStatus};
use serde_json::{Value, json};

use super::{HeldPilotWarnings, PilotWarningCandidacy};
use crate::OrbitRuntime;
use crate::application::task::pilot::input::{
    bounded_usize, requested_workspace_root, string_array,
};
use crate::application::task::pilot::source::resolve_source_snapshot;
use crate::application::task::{TaskListFilter, VerifierAssignment};

const DEFAULT_MAX_PARTITION_SIZE: usize = 5;
const HARD_MAX_PARTITION_SIZE: usize = 5;
const DEFAULT_MAX_TASKS: usize = 50;
const HARD_MAX_TASKS: usize = 500;

/// Refuting a pilot finding takes the strongest reading of the source, so
/// every triager is drawn from this tier.
const TRIAGE_TIER: TaskComplexity = TaskComplexity::Hard;

pub(crate) fn prepare(
    runtime: &OrbitRuntime,
    action: &str,
    input: &Value,
    run_id: Option<&str>,
) -> Result<Value, DispatchError> {
    let failed = |error: OrbitError| DispatchError::action_failed(action, error.to_string());
    let workspace_root = requested_workspace_root(runtime, action, input)?;
    let claim = crate::application::automation::members::claim(runtime, input).map_err(failed)?;
    // Triagers cite the source to dispute a finding, so the run pins one
    // revision as finding verification does.
    let source =
        resolve_source_snapshot(runtime, action, input, &workspace_root)?.ok_or_else(|| {
            DispatchError::action_failed(
                action,
                "escalation triage pins a Git revision; the workspace is not a Git work tree",
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
    let mut held = crate::application::automation::preparation::active_escalation_triages(runtime)
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
                    statuses: Some(vec![TaskStatus::Proposed]),
                    ..Default::default()
                },
                usize::MAX,
            )
            .map_err(failed)?
            .items;
        // Oldest holds first, so a capped pass makes progress through them.
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
    };

    // An explicitly named task that no longer qualifies settles superseded
    // with its reason, so a claimed member is released rather than failed.
    let mut superseded = Vec::new();
    let mut excluded = BTreeMap::<&'static str, usize>::new();
    let mut selected = Vec::new();
    for task in candidates {
        let candidacy = runtime.pilot_warning_candidacy_of(&task).map_err(failed)?;
        let reason = match (&candidacy, held.get(&task.id)) {
            (_, Some(_)) => Err("already_triaging"),
            (PilotWarningCandidacy::Qualifies(warnings), None) => Ok(warnings.clone()),
            (other, None) => Err(other.as_str()),
        };
        match reason {
            Ok(warnings) if selected.len() < max_tasks => selected.push((task, warnings)),
            Ok(_) => *excluded.entry("deferred_by_max_tasks").or_default() += 1,
            Err(reason) if explicit => superseded.push(json!({
                "task_id": task.id, "outcome": "superseded", "reason": reason,
                "status": task.status,
                "detail": skipped_detail(reason, held.get(&task.id)),
            })),
            // An unheld proposed task is not a candidate at all.
            Err("not_held") => {}
            Err(reason) => *excluded.entry(reason).or_default() += 1,
        }
    }

    // Every task's triager is drawn before any is dispatched, so its crew is
    // part of the prepared checkpoint the run records.
    let mut tasks = Vec::with_capacity(selected.len());
    let mut by_crew = BTreeMap::<String, Vec<String>>::new();
    for (task, warnings) in &selected {
        let author = assessment_author(runtime, warnings).map_err(failed)?;
        let triager = triager(runtime, task, author.as_deref()).map_err(failed)?;
        by_crew
            .entry(triager.crew.clone())
            .or_default()
            .push(task.id.clone());
        tasks.push(json!({
            "task_id": task.id,
            "title": task.title,
            "status": task.status,
            "operation_id": warnings.operation_id,
            "pilot_run_id": warnings.pilot_run_id,
            "findings": warnings.findings,
            "assessment_crew": author,
            "crew": triager.crew,
            "tier": triager.tier,
        }));
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
        "subject": "pilot_warning",
        "workspace_path": workspace_root,
        "owner_machine": runtime.automation_execution_location(),
        "source": source.to_json(),
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

/// The crew that wrote the held assessment: the pilot run's frozen draw.
/// `None` when the audit names no run, or the run's state is gone.
fn assessment_author(
    runtime: &OrbitRuntime,
    warnings: &HeldPilotWarnings,
) -> Result<Option<String>, OrbitError> {
    let Some(run_id) = &warnings.pilot_run_id else {
        return Ok(None);
    };
    Ok(runtime
        .read_run_state(run_id)?
        .as_ref()
        .and_then(crate::application::distributed::pilot_run_crew))
}

/// `task`'s triager: a hard-tier draw that can inspect source natively,
/// passing over the assessment's author while the pool offers another, else
/// the inspection-capable default crew, exactly as a finding verifier is
/// drawn. With neither, preparation fails with `inspection_crew_unavailable`
/// before any triager launches.
fn triager(
    runtime: &OrbitRuntime,
    task: &Task,
    author: Option<&str>,
) -> Result<VerifierAssignment, OrbitError> {
    let avoid = author.into_iter().collect::<Vec<_>>();
    if let Some(assignment) = runtime.draw_finding_verifier(task, TRIAGE_TIER, &avoid)? {
        return Ok(assignment);
    }
    let crew = runtime.inspection_capable_default_crew(TRIAGE_TIER)?;
    Ok(VerifierAssignment {
        crew: crew.name,
        tier: TRIAGE_TIER,
    })
}

fn skipped_detail(reason: &str, holders: Option<&BTreeSet<String>>) -> String {
    match reason {
        "already_triaging" => format!(
            "triaged by run {}",
            holders
                .into_iter()
                .flatten()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ),
        "not_held" => {
            "the task is not proposed, or its latest task-pilot assessment holds nothing on \
             warnings"
                .into()
        }
        "operator_decided" => {
            "an operator recorded a task-pilot admission decision on the latest assessment".into()
        }
        "triaged" => "the latest assessment was already triaged".into(),
        _ => reason.into(),
    }
}
