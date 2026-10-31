use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::{DateTime, Utc};
use orbit_store::contracts::JobRunQuery;
use orbit_types::task::{Task, TaskHistoryEntry, TaskStatus};
use serde_json::Value;

use super::super::records::{landed, run_spans};
use super::model::{ExecutionMode, IMPLEMENTATION_JOBS, RunRecord, Runner};
use crate::OrbitRuntime;

/// The status event a claim's pull writes when it opens an attempt on
/// another machine.
const PULLED_BY_EVENT: &str = "pulled_by";

/// The implementation job runs active since `horizon`, by the task their
/// input names, oldest first. Read without their steps.
pub(super) fn implementation_runs(
    runtime: &OrbitRuntime,
    horizon: DateTime<Utc>,
) -> BTreeMap<String, Vec<RunRecord>> {
    let mut by_task: BTreeMap<String, Vec<RunRecord>> = BTreeMap::new();
    for job in IMPLEMENTATION_JOBS {
        let runs = runtime
            .stores()
            .jobs()
            .list_job_runs_filtered(&JobRunQuery {
                job_id: Some((*job).to_string()),
                recency_since: Some(horizon),
                include_steps: false,
                ..Default::default()
            })
            .unwrap_or_else(|error| {
                tracing::warn!(job, %error, "agent scorecards: implementation runs unreadable");
                Vec::new()
            });
        for run in runs {
            let Some(input) = run.input.as_ref() else {
                continue;
            };
            let named: BTreeSet<&str> = input
                .get("task_ids")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .chain(input.get("task_id").and_then(Value::as_str))
                .collect();
            for task_id in named {
                by_task
                    .entry(task_id.to_string())
                    .or_default()
                    .push(RunRecord {
                        started_at: run.started_at.unwrap_or(run.created_at),
                        finished_at: run.finished_at,
                        runner: Runner {
                            crew: run.resolved_crew.clone(),
                            model: run.crew_model.clone(),
                            run_id: Some(run.run_id.to_string()),
                        },
                    });
            }
        }
    }
    for runs in by_task.values_mut() {
        runs.sort_by_key(|run| run.started_at);
    }
    by_task
}

/// The job run that worked an attempt starting at `start`: the latest run
/// that started by then and had not finished, else the first that started
/// inside the attempt. `None` when no local run worked it.
pub(super) fn runner(
    runs: &[RunRecord],
    start: DateTime<Utc>,
    end: Option<DateTime<Utc>>,
) -> Option<Runner> {
    runs.iter()
        .rev()
        .find(|run| run.started_at <= start && run.finished_at.is_none_or(|done| done >= start))
        .or_else(|| {
            runs.iter()
                .find(|run| run.started_at >= start && end.is_none_or(|end| run.started_at <= end))
        })
        .map(|run| run.runner.clone())
}

/// Who worked an attempt with no local job run, as a pull from another
/// machine is: the crew and model on the settlement record of the claim the
/// `pulled_by` note names, from `pulled` by claim, and the pulling run that
/// note names. Without a record, or a field it lacks, the crew falls back to
/// the one task history had assigned by the attempt's start (else the task's
/// crew when history records no assignment) and the model stays unknown.
pub(super) fn assigned_runner(
    task: &Task,
    history: &[TaskHistoryEntry],
    pulled: &HashMap<&str, Runner>,
    start: DateTime<Utc>,
) -> Runner {
    let note = history
        .iter()
        .find(|entry| entry.at == start && entry.event == PULLED_BY_EVENT)
        .and_then(|entry| entry.note.as_deref())
        .and_then(|note| serde_json::from_str::<Value>(note).ok());
    let field = |pointer: &str| {
        note.as_ref()
            .and_then(|note| note.pointer(pointer))
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let settled = field("/claim_id").and_then(|claim| pulled.get(claim.as_str()));
    let assignments = || {
        history
            .iter()
            .filter(|entry| matches!(entry.event.as_str(), "crew_assigned" | "crew_redrawn"))
    };
    let crew = match assignments().rfind(|entry| entry.at <= start) {
        Some(entry) => entry.note.as_deref().and_then(assigned_crew),
        None if assignments().next().is_none() => task.crew.clone(),
        None => None,
    };
    Runner {
        crew: settled.and_then(|runner| runner.crew.clone()).or(crew),
        model: settled.and_then(|runner| runner.model.clone()),
        run_id: field("/run_context/run_id"),
    }
}

/// The crew an assignment note ends on: its last backticked name, which every
/// assignment and redraw note gives last. A placeholder such as `(none)` is no
/// crew.
pub(super) fn assigned_crew(note: &str) -> Option<String> {
    let last = note.split('`').skip(1).step_by(2).last()?;
    (!last.is_empty() && !last.starts_with(['(', '<'])).then(|| last.to_string())
}

/// Whether a claim's pull opened the attempt starting at `start`: a job on
/// another machine worked it, even when the pull recorded no run id.
pub(super) fn opened_by_claim(history: &[TaskHistoryEntry], start: DateTime<Utc>) -> bool {
    history
        .iter()
        .any(|entry| entry.at == start && entry.event == PULLED_BY_EVENT)
}

/// When the task last moved to done.
pub(super) fn landed_at(history: &[TaskHistoryEntry]) -> Option<DateTime<Utc>> {
    history
        .iter()
        .rev()
        .find(|entry| entry.to_status == Some(TaskStatus::Done))
        .map(|entry| entry.at)
}

/// The attempt whose candidate landed: the last one started before the task
/// was done, else the last one. `None` for a task that did not land.
pub(super) fn final_attempt(
    landed: bool,
    landed_at: Option<DateTime<Utc>>,
    starts: &[DateTime<Utc>],
) -> Option<usize> {
    if !landed {
        return None;
    }
    match landed_at {
        Some(at) => starts.iter().rposition(|start| *start <= at),
        None => starts.len().checked_sub(1),
    }
}

/// How a task was executed, from whether a job worked each attempt (`jobs`,
/// in start order): managed when its final attempt (the landed one, else the
/// latest) was a job's, or when history records no attempt but an
/// implementation run (`job_run`) names the task; interactive when it reached
/// review or done without that; unclassified otherwise.
pub(super) fn execution_mode(
    status: TaskStatus,
    history: &[TaskHistoryEntry],
    jobs: &[bool],
    final_attempt: Option<usize>,
    job_run: bool,
) -> ExecutionMode {
    let managed = match final_attempt.or(jobs.len().checked_sub(1)) {
        Some(index) => jobs[index],
        None => job_run,
    };
    if managed {
        return ExecutionMode::Managed;
    }
    let reviewed = |status: TaskStatus| matches!(status, TaskStatus::Review | TaskStatus::Done);
    if reviewed(status)
        || history
            .iter()
            .any(|entry| entry.to_status.is_some_and(reviewed))
    {
        ExecutionMode::Interactive
    } else {
        ExecutionMode::Unclassified
    }
}

/// The execution mode of each of `tasks` (with its history), derived as the
/// agent scan derives it, from the implementation job runs active since
/// `since`. For readers outside the scan, such as experiment readouts.
pub(crate) fn task_execution_modes(
    runtime: &OrbitRuntime,
    since: DateTime<Utc>,
    tasks: &[(&Task, &[TaskHistoryEntry])],
    now: DateTime<Utc>,
) -> BTreeMap<String, ExecutionMode> {
    let mut runs = implementation_runs(runtime, since);
    tasks
        .iter()
        .map(|(task, history)| {
            let id = task.id.to_string();
            let task_runs = runs.remove(&id).unwrap_or_default();
            let spans = run_spans(history);
            let jobs: Vec<bool> = spans
                .iter()
                .map(|span| {
                    runner(&task_runs, span.start, span.end.or(Some(now))).is_some()
                        || opened_by_claim(history, span.start)
                })
                .collect();
            let starts: Vec<_> = spans.iter().map(|span| span.start).collect();
            let landed = landed(task.status, history);
            let last = final_attempt(landed, landed_at(history), &starts);
            let mode = execution_mode(task.status, history, &jobs, last, !task_runs.is_empty());
            (id, mode)
        })
        .collect()
}
