use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::{DateTime, Utc};
use orbit_store::contracts::JobRunQuery;
use orbit_types::task::{RecordedCrewAssignment, Task, TaskHistoryEntry, TaskStatus};
use serde_json::Value;

use super::super::records::{landed, run_spans};
use super::model::{ExecutionMode, RunRecord, Runner};
use crate::OrbitRuntime;

/// The status event a claim's pull writes when it opens an attempt on
/// another machine.
const PULLED_BY_EVENT: &str = "pulled_by";

/// The implementation job runs active since `horizon`, by the task their
/// input names, oldest first. Read without their steps, one query per
/// delivery leaf on its own thread: each leaf's history is an independent
/// read through the store's pooled readers, and a scan reads all four.
pub(super) fn implementation_runs(
    runtime: &OrbitRuntime,
    horizon: DateTime<Utc>,
) -> BTreeMap<String, Vec<RunRecord>> {
    // The jobs whose runs implement a task: the PR and local delivery
    // leaves, owned or claimed.
    let per_job: Vec<Vec<(String, RunRecord)>> = std::thread::scope(|scope| {
        let handles: Vec<_> = orbit_types::workflow::DELIVERY_LEAF_JOBS
            .iter()
            .map(|job| scope.spawn(move || delivery_leaf_runs(runtime, job, horizon)))
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap_or_default())
            .collect()
    });
    let mut by_task: BTreeMap<String, Vec<RunRecord>> = BTreeMap::new();
    for (task_id, record) in per_job.into_iter().flatten() {
        by_task.entry(task_id).or_default().push(record);
    }
    for runs in by_task.values_mut() {
        runs.sort_by_key(|run| run.started_at);
    }
    by_task
}

/// One delivery leaf's runs since `horizon`, as (task, record) pairs in
/// the order the store listed the runs.
fn delivery_leaf_runs(
    runtime: &OrbitRuntime,
    job: &str,
    horizon: DateTime<Utc>,
) -> Vec<(String, RunRecord)> {
    let runs = runtime
        .stores()
        .jobs()
        .list_job_runs_filtered(&JobRunQuery {
            job_id: Some(job.to_string()),
            recency_since: Some(horizon),
            include_steps: false,
            // Only the task links are read from each run's input.
            input_keys: Some(&["task_id", "task_ids"]),
            ..Default::default()
        })
        .unwrap_or_else(|error| {
            tracing::warn!(job, %error, "agent scorecards: implementation runs unreadable");
            Vec::new()
        });
    let mut records = Vec::new();
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
            records.push((
                task_id.to_string(),
                RunRecord {
                    started_at: run.started_at.unwrap_or(run.created_at),
                    finished_at: run.finished_at,
                    runner: Runner {
                        crew: run.resolved_crew.clone(),
                        model: run.crew_model.clone(),
                        run_id: Some(run.run_id.to_string()),
                        reported_effort: None,
                    },
                },
            ));
        }
    }
    records
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

/// What the claims other machines pulled record of who ran them, read once
/// per scan.
#[derive(Debug, Default)]
pub(in crate::application::agents) struct PulledClaimRunners<'a> {
    /// The crew, model and reported effort of each settled claim, by claim.
    pub(super) by_claim: HashMap<&'a str, Runner>,
    /// Each task's settled claims, so a stored row's freshness depends on
    /// the claims its own task was pulled under, not every claim's.
    pub(super) by_task: HashMap<&'a str, Vec<&'a str>>,
    /// The models each crew is known to have run.
    pub(super) crew_models: KnownCrewModels,
}

/// The models each crew is known to have run: every crew and model one record
/// names together, which is a local implementation run, a claim settlement or
/// a follower's late report that names its crew. The crew and the model of
/// such a record describe one leaf, so the pair is what actually ran.
///
/// The crew definitions are not used. They say what a crew resolves to on
/// this host now, and a follower's configuration, or an earlier one of this
/// host, can resolve the same crew to another model; the records keep what
/// each crew ran when it ran. A crew no record pairs with a model has no
/// known model.
#[derive(Debug, Default)]
pub(in crate::application::agents) struct KnownCrewModels(BTreeMap<String, BTreeSet<String>>);

impl KnownCrewModels {
    /// Note the pair a record names, when it names both.
    pub(super) fn note_recorded_pair(&mut self, runner: &Runner) {
        if let (Some(crew), Some(model)) = (&runner.crew, &runner.model) {
            self.0
                .entry(crew.clone())
                .or_default()
                .insert(model.clone());
        }
    }

    /// Whether a record shows `crew` running `model`.
    fn ran(&self, crew: &str, model: &str) -> bool {
        self.0
            .get(crew)
            .is_some_and(|models| models.contains(model))
    }

    /// Whether the records show `crew` running models and `model` is none of
    /// them. False for a crew with no known model.
    fn contradicts(&self, crew: &str, model: &str) -> bool {
        self.0
            .get(crew)
            .is_some_and(|models| !models.contains(model))
    }
}

/// Who worked an attempt with no local job run, as a pull from another
/// machine is: the crew and model of the claim the `pulled_by` note names,
/// from `pulled` by claim, and the pulling run that note names. That record is
/// the settlement or, for a claim settled without one, the follower's late
/// report [ORB-24206]. When that record names a crew, the attempt is keyed to
/// that crew with the record's model. When it names no crew (a follower that
/// predates the crew field), the crew falls back to the one task history had
/// assigned by the attempt's start (else the task's crew when history records
/// no assignment), and the record's model still counts as the record gave it.
/// That fallback pairs a crew and a model from two records, so it is dropped
/// when they contradict each other [ORB-24967]: when the history crew is
/// known to run models ([`KnownCrewModels`]) and the record's model is none
/// of them, the attempt is keyed to the one crew the task's crew assignments
/// name that is known to run that model, else to no crew. A history crew with
/// no known model, or one known to run the record's model, stands.
/// Without a record the model stays unknown. The record's
/// reported reasoning effort rides along for the effort lookup's fallback; it
/// is never derived from this owner's crew configuration.
pub(super) fn assigned_runner(
    task: &Task,
    history: &[TaskHistoryEntry],
    pulled: &PulledClaimRunners<'_>,
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
    let settled = field("/claim_id").and_then(|claim| pulled.by_claim.get(claim.as_str()));
    let assignments: Vec<(DateTime<Utc>, RecordedCrewAssignment)> = history
        .iter()
        .filter_map(|entry| {
            RecordedCrewAssignment::from_entry(entry).map(|assignment| (entry.at, assignment))
        })
        .collect();
    let history_crew = match assignments.iter().rfind(|(at, _)| *at <= start) {
        Some((_, assignment)) => assignment.known_after_crew().map(str::to_string),
        None if assignments.is_empty() => task.crew.clone(),
        None => None,
    };
    let model = settled.and_then(|runner| runner.model.clone());
    let crew = match settled.and_then(|runner| runner.crew.clone()) {
        Some(recorded) => Some(recorded),
        None => match (history_crew, model.as_deref()) {
            (Some(crew), Some(model)) if pulled.crew_models.contradicts(&crew, model) => {
                let mut running: BTreeSet<&str> = assignments
                    .iter()
                    .flat_map(|(_, assignment)| {
                        [
                            assignment.known_before_crew(),
                            assignment.known_after_crew(),
                        ]
                    })
                    .flatten()
                    .chain(task.crew.as_deref())
                    .filter(|crew| pulled.crew_models.ran(crew, model))
                    .collect();
                match (running.pop_first(), running.is_empty()) {
                    (Some(only), true) => Some(only.to_string()),
                    _ => None,
                }
            }
            (crew, _) => crew,
        },
    };
    Runner {
        crew,
        model,
        run_id: field("/run_context/run_id"),
        reported_effort: settled.and_then(|runner| runner.reported_effort.clone()),
    }
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
