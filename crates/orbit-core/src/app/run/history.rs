use super::{Command, Operation, RunRead};
use crate::OrbitRuntime;
use crate::application::job::{
    JobRunListParams, JobRunListStates, fold_run_incidents, job_run_task_ids, run_error_step,
};
use orbit_types::workflow::{JobRun, JobRunState, JobTargetType, PipelineState, RunRootCause};
use std::collections::{HashMap, HashSet};

pub struct RunHistory {
    pub runs: Vec<JobRun>,
    pub states: JobRunListStates,
    pub failures: Option<(Vec<FailureRow>, usize)>,
}

impl Command<'static, RunHistory> {
    pub fn history(params: JobRunListParams, read: RunRead, failures: bool) -> Self {
        Self::new(
            Operation::command("run", "history").target(Some("job_run"), params.job_id.clone()),
            move |core| {
                let rt = core.runtime();
                let runs = read.list(rt, params)?;
                let states = rt.read_job_run_list_states(&runs)?;
                let failures =
                    failures.then(|| failure_rows(rt, read, &runs, &states.for_runs(&runs)));
                Ok(RunHistory {
                    runs,
                    states,
                    failures,
                })
            },
        )
    }
}
/// One `--failures` row: a task and the root-cause run behind it.
pub struct FailureRow {
    pub task_id: Option<String>,
    pub run_id: String,
    pub step: Option<String>,
    pub crew: Option<String>,
    pub cause: Option<String>,
}

/// Fold the listing into one row per task and root cause.
///
/// Echoes of the same leaf collapse, including a held or interrupted parent.
/// An upgrade's designed pause is left out and only counted. The cause is the root-cause run's own error, first line only, with no
/// character cap. `--limit` still bounds the runs scanned.
fn failure_rows(
    runtime: &OrbitRuntime,
    read: RunRead,
    runs: &[JobRun],
    states: &[Option<&PipelineState>],
) -> (Vec<FailureRow>, usize) {
    let listed = runs
        .iter()
        .enumerate()
        .map(|(index, run)| (run.run_id.as_str(), index))
        .collect::<HashMap<_, _>>();
    let mut loaded: HashMap<String, FailureFacts> = HashMap::new();
    let mut seen = HashSet::new();
    let mut rows = Vec::new();
    let paused_leaves = fold_run_incidents(runs, states)
        .into_iter()
        .filter(|incident| incident.upgrade_paused)
        .map(|incident| incident.leaf_run_id)
        .collect::<HashSet<_>>();
    for (index, run) in runs.iter().enumerate() {
        if !is_reported_failure(run.state) {
            continue;
        }
        let recorded = states
            .get(index)
            .copied()
            .flatten()
            .and_then(|state| state.root_cause.as_ref());
        let root_id = recorded
            .map(|cause| cause.leaf_run_id.as_str())
            .unwrap_or(run.run_id.as_str());
        if paused_leaves.contains(root_id) {
            continue;
        }
        let (mut task_ids, mut step, crew, cause) =
            root_facts(runtime, read, runs, &listed, &mut loaded, root_id);
        if step.is_none() {
            step = recorded.and_then(|cause| cause.step.clone());
        }
        if task_ids.is_empty() {
            task_ids = recorded_or_echo_tasks(run, recorded);
        }
        let tasks = if task_ids.is_empty() {
            vec![None]
        } else {
            task_ids.into_iter().map(Some).collect::<Vec<_>>()
        };
        for task_id in tasks {
            let key = (task_id.clone().unwrap_or_default(), root_id.to_string());
            if !seen.insert(key) {
                continue;
            }
            rows.push(FailureRow {
                task_id,
                run_id: root_id.to_string(),
                step: step.clone(),
                crew: crew.clone(),
                cause: cause.clone(),
            });
        }
    }

    (rows, paused_leaves.len())
}
fn is_reported_failure(state: JobRunState) -> bool {
    matches!(
        state,
        JobRunState::Failed | JobRunState::Held | JobRunState::Interrupted | JobRunState::Timeout
    )
}

/// Task ids, failing step, crew and first cause line of the root-cause run.
type FailureFacts = (Vec<String>, Option<String>, Option<String>, Option<String>);

/// Cache each root's facts so folded echoes share one checkpoint/audit read.
/// Missing runs contribute nothing; the caller fills step and tasks from the
/// recorded cause.
fn root_facts(
    runtime: &OrbitRuntime,
    read: RunRead,
    runs: &[JobRun],
    listed: &HashMap<&str, usize>,
    loaded: &mut HashMap<String, FailureFacts>,
    root_id: &str,
) -> FailureFacts {
    loaded
        .entry(root_id.to_string())
        .or_insert_with(|| {
            if let Some(&index) = listed.get(root_id) {
                return facts_of(runtime, &runs[index]);
            }
            read.show(runtime, root_id)
                .ok()
                .as_ref()
                .map(|run| facts_of(runtime, run))
                .unwrap_or_default()
        })
        .clone()
}

fn facts_of(runtime: &OrbitRuntime, run: &JobRun) -> FailureFacts {
    let step = runtime
        .run_root_cause(&run.run_id)
        .ok()
        .flatten()
        .and_then(|cause| cause.step)
        .or_else(|| {
            // Keep activity-row evidence if the checkpoint cannot be read.
            run_error_step(run)
                .filter(|step| step.target_type != JobTargetType::Job)
                .map(|step| step.target_id.clone())
        });
    (
        job_run_task_ids(run),
        step,
        run.resolved_crew.clone(),
        failure_cause_line(run),
    )
}

fn recorded_or_echo_tasks(run: &JobRun, recorded: Option<&RunRootCause>) -> Vec<String> {
    if let Some(task_id) = recorded.and_then(|cause| cause.task_id.clone()) {
        return vec![task_id];
    }
    job_run_task_ids(run)
}

/// The root-cause run's error on one line. Later lines stay off the row, and
/// nothing here applies the history table's 60-character cap.
fn failure_cause_line(run: &JobRun) -> Option<String> {
    let step = run_error_step(run)?;
    let text = match (step.error_code.as_deref(), step.error_message.as_deref()) {
        (Some(code), Some(message)) => format!("{code}: {}", first_line(message)),
        (Some(text), None) | (None, Some(text)) => first_line(text),
        (None, None) => return None,
    };
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// The first line of `text`, with whitespace runs collapsed so a cell cannot
/// split a tab-separated row.
fn first_line(text: &str) -> String {
    text.lines()
        .next()
        .unwrap_or("")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
