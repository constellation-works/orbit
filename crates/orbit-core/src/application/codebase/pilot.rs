//! Task-pilot receipts and the evidence the Agents › Pilots scorecard grades:
//! each done task a pilot assessed, with its landed diff, the pilot's and the
//! filer's selectors and the paths its run was contested on, and the lock
//! waits other tasks spent on paths each such task held.
//!
//! A task's pilot selectors are the newest task-pilot receipt's
//! `context_files_after`; the filer's are the oldest receipt's
//! `context_files_before`, what the task declared before any pilot ran. The
//! receipt names its pilot job run (`pilot_run_id`), whose record holds the
//! frozen activity crew draw; invocation evidence holds the pilot model. A
//! receipt written before that field is matched to the one task-pilot run
//! active when it was written, or among several, the one whose apply report
//! carries its exact operation id. Models come from the producing worker step
//! when its apply report is available. Without a report, only a single
//! recorded worker can identify the round (including a repair-only run).
//! Task links distinguish fan-out invocations; ambiguous evidence stays unknown.

use std::cell::RefCell;
use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::{InvocationQuery, InvocationRecord, JobRunQuery};
use orbit_types::workflow::{JobRun, JobV2Step, JobV2StepBody};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::facts::{Anchor, BlockCause, CodebaseFacts};
use super::lock_waits::wait_episodes;
use super::regression::RegressionFilter;
use super::sections::{LARGE_FOOTPRINT_CAP, Params, Rollup};
use crate::OrbitRuntime;

/// The actor that writes task-pilot receipts.
const PILOT_ACTOR: &str = "task-pilot";
/// The job every task-pilot run (scheduled, drain or CI sweep) belongs to.
const PILOT_JOB: &str = "task_pilot_pipeline";
/// The pilot run behind an assessment: its id, the crew and model its
/// activity recorded (either `None` when it recorded none) and how long it ran.
#[derive(Debug, Clone, Default, PartialEq)]
pub(in crate::application) struct PilotRun {
    pub(in crate::application) run_id: String,
    pub(in crate::application) crew: Option<String>,
    pub(in crate::application) model: Option<String>,
    /// Start to finish; `None` while it runs or when either is unrecorded.
    pub(in crate::application) seconds: Option<f64>,
}

struct RunRecord {
    started_at: Option<DateTime<Utc>>,
    finished_at: Option<DateTime<Utc>>,
    run: PilotRun,
    /// Loaded once per attributed run, shared across its tasks and receipts.
    invocations: RefCell<Option<Vec<InvocationRecord>>>,
}

impl RunRecord {
    fn model_for_pilot_receipt(
        &self,
        runtime: &OrbitRuntime,
        steps: &[(String, String)],
        producing: &[usize],
        task_id: &str,
    ) -> Option<String> {
        let mut invocations = self.invocations.borrow_mut();
        let invocations = invocations.get_or_insert_with(|| {
            // A truncated or failed read cannot establish an identity.
            const LIMIT: usize = 1000;
            match runtime.workspace_invocation_records(InvocationQuery {
                job_run_id: Some(self.run.run_id.clone()),
                limit: LIMIT,
                ..Default::default()
            }) {
                Ok(records) if records.len() < LIMIT => records,
                Ok(_) => {
                    tracing::warn!(run_id = %self.run.run_id, "pilot identity: invocation read truncated");
                    Vec::new()
                }
                Err(error) => {
                    tracing::warn!(run_id = %self.run.run_id, %error, "pilot identity: invocations unreadable");
                    Vec::new()
                }
            }
        });
        let workers: Vec<_> = steps
            .iter()
            .enumerate()
            .filter_map(|(index, (_, worker))| {
                let matches = match producing {
                    [producing] => *producing == index,
                    [] => invocations.iter().any(|invocation| {
                        invocation.activity_id == *worker
                            && (invocation.task_ids.is_empty()
                                || invocation.task_ids.iter().any(|id| id == task_id))
                    }),
                    _ => false,
                };
                matches.then_some(worker)
            })
            .collect();
        let [worker] = workers.as_slice() else {
            return None;
        };
        let recorded: Vec<_> = invocations
            .iter()
            .filter(|invocation| invocation.activity_id == **worker)
            .collect();
        // Records are newest first. A task-linked retry supersedes its
        // previous invocation, even when its model is unrecorded.
        if let Some(invocation) = recorded
            .iter()
            .find(|invocation| invocation.task_ids.iter().any(|id| id == task_id))
        {
            return invocation.model.clone();
        }
        // Legacy unlinked evidence is usable only when unique.
        match recorded.as_slice() {
            [invocation] if invocation.task_ids.is_empty() => invocation.model.clone(),
            _ => None,
        }
    }
}

/// The workspace's task-pilot runs, read once per scan without their steps:
/// what every receipt, held or clean, is attributed to.
pub(in crate::application) struct PilotRuns {
    runs: Vec<RunRecord>,
    steps: Vec<(String, String)>,
}

impl PilotRuns {
    pub(in crate::application) fn load(runtime: &OrbitRuntime) -> Self {
        let runs = runtime
            .stores()
            .jobs()
            .list_job_runs_filtered(&JobRunQuery {
                job_id: Some(PILOT_JOB.to_string()),
                include_steps: false,
                ..Default::default()
            })
            .unwrap_or_else(|error| {
                tracing::warn!(%error, "pilot scorecard: task-pilot runs unreadable");
                Vec::new()
            });
        Self::from_runs(runtime, runs)
    }

    /// Detail reads with linked receipts need only their recorded runs. A
    /// legacy receipt without a run id still needs the overlapping-run scan.
    pub(in crate::application) fn load_for_receipts<'a>(
        runtime: &OrbitRuntime,
        run_ids: impl Iterator<Item = Option<&'a str>>,
    ) -> Self {
        let Some(ids) = run_ids.collect::<Option<BTreeSet<_>>>() else {
            return Self::load(runtime);
        };
        let runs = ids
            .into_iter()
            .filter_map(|id| match runtime.get_job_run_backend(id) {
                Ok(Some(run)) if run.job_id == PILOT_JOB => Some(run),
                Ok(_) => None,
                Err(error) => {
                    tracing::warn!(run_id = id, %error, "pilot identity: run unreadable");
                    None
                }
            })
            .collect();
        Self::from_runs(runtime, runs)
    }

    fn from_runs(runtime: &OrbitRuntime, runs: Vec<JobRun>) -> Self {
        let crews = runtime
            .recorded_run_crews(runs.iter())
            .unwrap_or_else(|error| {
                tracing::warn!(%error, "pilot scorecard: recorded crews unreadable");
                Default::default()
            });
        // Worker ids, rather than activity catalog names, name invocations.
        let steps = pilot_steps(runtime);
        let runs = runs
            .into_iter()
            .map(|run| RunRecord {
                started_at: run.started_at,
                finished_at: run.finished_at,
                run: PilotRun {
                    run_id: run.run_id.to_string(),
                    crew: crews.crews(&run, None).into_iter().next(),
                    model: None,
                    seconds: run.started_at.zip(run.finished_at).map(|(start, end)| {
                        (end - start).num_milliseconds().max(0) as f64 / 1000.0
                    }),
                },
                invocations: RefCell::new(None),
            })
            .collect();
        Self { runs, steps }
    }

    /// Explicit run ids win. Legacy receipts use a unique active run, or
    /// disambiguate overlapping runs by exact operation id in an apply report.
    pub(in crate::application) fn attribute(
        &self,
        runtime: &OrbitRuntime,
        task_id: &str,
        run_id: Option<&str>,
        at: DateTime<Utc>,
        operation_id: &str,
        now: DateTime<Utc>,
    ) -> Option<PilotRun> {
        let candidates: Vec<_> = self
            .runs
            .iter()
            .filter(|record| {
                if let Some(run_id) = run_id {
                    record.run.run_id == run_id
                } else {
                    record.started_at.is_some_and(|started| started <= at)
                        && record.finished_at.unwrap_or(now) >= at
                }
            })
            .collect();
        let mut found = candidates.iter().filter_map(|record| {
            let state = runtime.read_run_state(&record.run.run_id).ok().flatten();
            let producing: Vec<_> = self
                .steps
                .iter()
                .enumerate()
                .filter_map(|(index, (step, _))| {
                    let tasks = state.as_ref()?.pipeline_step_output(step)?["tasks"].as_array()?;
                    let matches = tasks
                        .iter()
                        .any(|task| task["operation_id"].as_str() == Some(operation_id));
                    matches.then_some(index)
                })
                .collect();
            if candidates.len() > 1 && producing.len() != 1 {
                return None;
            }
            let mut run = record.run.clone();
            run.model = record.model_for_pilot_receipt(runtime, &self.steps, &producing, task_id);
            Some(run)
        });
        let run = found.next()?;
        found.next().is_none().then_some(run)
    }
}

/// Apply step ids paired with their pilot worker ids, in pipeline order.
/// Read the job definition so renaming a worker or apply step preserves attribution.
fn pilot_steps(runtime: &OrbitRuntime) -> Vec<(String, String)> {
    let job = match runtime.resolved_job_spec(PILOT_JOB) {
        Ok(job) => job,
        Err(error) => {
            tracing::warn!(%error, "pilot scorecard: task-pilot definition unreadable");
            return Vec::new();
        }
    };
    job.steps
        .iter()
        .filter_map(|apply| {
            let input = activity_input(apply, "apply_task_pilot_results")?;
            let results = input.get("results")?.as_str()?;
            let worker = job.steps.iter().find_map(|step| {
                let JobV2StepBody::FanOut { fan_out, fan_in } = &step.body else {
                    return None;
                };
                activity_input(&fan_out.worker, "task_pilot")?;
                let collect = fan_in.collect.as_deref().unwrap_or(&step.id);
                (results == format!("{{{{ steps.{collect}.output }}}}"))
                    .then_some(&fan_out.worker.id)
            })?;
            Some((apply.id.clone(), worker.clone()))
        })
        .collect()
}

fn activity_input<'a>(step: &'a JobV2Step, name: &str) -> Option<&'a Value> {
    match &step.body {
        JobV2StepBody::Target(target) if target.activity_name.as_deref() == Some(name) => {
            target.default_input.as_ref()
        }
        JobV2StepBody::TargetRef(target) if target.target == format!("activity:{name}") => {
            target.default_input.as_ref()
        }
        _ => None,
    }
}

fn strings(value: &Value) -> Option<Vec<String>> {
    value.as_array().map(|values| {
        values
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect()
    })
}

/// What a task's task-pilot receipts record, before the newest one is
/// attributed to a run: kept with the task's stored facts, since only its
/// comments change it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct PilotInputs {
    /// The newest assessment's final selectors.
    after: Vec<String>,
    /// The filer's selectors before any pilot ran.
    author: Vec<String>,
    /// The pilot run the newest assessment names, when it names one.
    run_id: Option<String>,
    /// When the newest assessment was written, and its operation id: what a
    /// receipt without a run id is matched by.
    at: DateTime<Utc>,
    operation_id: String,
}

/// Read a task's task-pilot receipts: `None` when no receipt carries the
/// pilot's final selectors, an error when the comments cannot be read.
pub(super) fn inputs(runtime: &OrbitRuntime, id: &str) -> Result<Option<PilotInputs>, OrbitError> {
    let comments = runtime.get_task_comments(id)?;
    // Comments are stored oldest first.
    let receipts: Vec<_> = comments
        .iter()
        .filter(|comment| comment.by == PILOT_ACTOR)
        .filter_map(|comment| {
            let receipt = comment.receipt()?;
            let body: Value = serde_json::from_str(receipt.body).ok()?;
            Some((comment.at, receipt.operation_id, body))
        })
        .collect();
    let Some((at, operation_id, newest)) = receipts
        .iter()
        .rev()
        .find(|(_, _, body)| body["assessment"]["context_files_after"].is_array())
    else {
        return Ok(None);
    };
    let Some(after) = strings(&newest["assessment"]["context_files_after"]) else {
        return Ok(None);
    };
    let author = receipts
        .first()
        .and_then(|(_, _, oldest)| {
            strings(&oldest["context_files_before"])
                .or_else(|| strings(&oldest["assessment"]["context_files_before"]))
        })
        .unwrap_or_default();
    Ok(Some(PilotInputs {
        after,
        author,
        run_id: newest["pilot_run_id"].as_str().map(str::to_string),
        at: *at,
        operation_id: operation_id.to_string(),
    }))
}

/// One done task a pilot assessed: what its receipts and landed diff say,
/// and the pilot run behind its newest assessment.
#[derive(Debug, Clone)]
pub(in crate::application) struct Assessment {
    pub(in crate::application) task_id: String,
    /// When the newest assessment was written: the window it falls in.
    pub(in crate::application) at: DateTime<Utc>,
    /// `None` when no task-pilot run is found for the receipt.
    pub(in crate::application) run: Option<PilotRun>,
    /// The pilot's final selectors.
    pub(in crate::application) after: Vec<String>,
    /// The filer's selectors before any pilot ran.
    pub(in crate::application) author: Vec<String>,
    /// The files the landed commit changed.
    pub(in crate::application) landed: BTreeSet<String>,
    /// Paths the task's own work was contested on: those a base-conflict
    /// block recorded and those a refused lock request of the task named,
    /// each with when and which.
    pub(in crate::application) contested: Vec<Contest>,
}

/// One path a task's work was contested on.
#[derive(Debug, Clone)]
pub(in crate::application) struct Contest {
    pub(in crate::application) at: DateTime<Utc>,
    pub(in crate::application) path: Anchor,
    /// `base conflict` or `lock conflict`.
    pub(in crate::application) kind: &'static str,
}

/// One lock-wait episode another task spent on a path an assessed task held.
#[derive(Debug, Clone)]
pub(in crate::application) struct HeldWait {
    /// The assessed task holding the path.
    pub(in crate::application) holder: String,
    pub(in crate::application) waiter: String,
    pub(in crate::application) path: Anchor,
    pub(in crate::application) started_at: DateTime<Utc>,
    pub(in crate::application) wait_ms: i64,
    /// How the wait ended: `admitted`, `left_backlog` or `waiting`.
    pub(in crate::application) end: &'static str,
}

/// What the pilots scorecard reads from one codebase scan.
#[derive(Debug, Clone, Default)]
pub(in crate::application) struct PilotEvidence {
    pub(in crate::application) assessments: Vec<Assessment>,
    pub(in crate::application) waits: Vec<HeldWait>,
}

/// Every done task with a landing receipt and a pilot assessment, apart
/// from bulk tasks (more than [`LARGE_FOOTPRINT_CAP`] landed files) and
/// `no-diff-expected` ones, each attributed to its pilot run, and every lock
/// wait, whenever it happened, on a path one of them held. `runs` is loaded
/// on first use and left for the caller to attribute other receipts with.
pub(in crate::application) fn pilot_evidence(
    runtime: &OrbitRuntime,
    facts: &CodebaseFacts,
    runs: &mut Option<PilotRuns>,
) -> PilotEvidence {
    let mut assessments = Vec::new();
    for task in &facts.tasks {
        let (Some(landed), Some(inputs)) = (&task.landed, &task.pilot) else {
            continue;
        };
        if task.no_diff || landed.paths.len() > LARGE_FOOTPRINT_CAP {
            continue;
        }
        let runs = runs.get_or_insert_with(|| PilotRuns::load(runtime));
        let run = runs.attribute(
            runtime,
            &task.id,
            inputs.run_id.as_deref(),
            inputs.at,
            &inputs.operation_id,
            facts.computed_at,
        );
        let mut contested: Vec<Contest> = task
            .blocks
            .iter()
            .filter_map(|block| match &block.cause {
                BlockCause::BaseConflict(paths) => Some((block.at, paths)),
                _ => None,
            })
            .flat_map(|(at, paths)| {
                paths.iter().map(move |path| Contest {
                    at,
                    path: path.clone(),
                    kind: "base conflict",
                })
            })
            .collect();
        for denial in &facts.lock_denials {
            if denial.task_ids.contains(&task.id) {
                contested.extend(denial.files.iter().map(|path| Contest {
                    at: denial.at,
                    path: path.clone(),
                    kind: "lock conflict",
                }));
            }
        }
        assessments.push(Assessment {
            task_id: task.id.clone(),
            at: inputs.at,
            run,
            after: inputs.after.clone(),
            author: inputs.author.clone(),
            landed: landed.paths.clone(),
            contested,
        });
    }
    let assessed: BTreeSet<&str> = assessments
        .iter()
        .map(|assessment| assessment.task_id.as_str())
        .collect();
    let every_wait = Params {
        since: None,
        min_support: 1,
        rollup: Rollup::File,
        filter: RegressionFilter::default(),
    };
    let mut waits = Vec::new();
    for (key, episode) in wait_episodes(facts, &every_wait) {
        let path = Anchor {
            path: key.trim_end_matches('/').to_string(),
            dir: key.ends_with('/'),
        };
        for holder in episode
            .holders
            .iter()
            .filter(|holder| assessed.contains(holder.as_str()))
        {
            waits.push(HeldWait {
                holder: holder.clone(),
                waiter: episode.task_id.clone(),
                path: path.clone(),
                started_at: episode.started_at,
                wait_ms: episode.wait_ms(),
                end: episode.end.as_str(),
            });
        }
    }
    PilotEvidence { assessments, waits }
}
