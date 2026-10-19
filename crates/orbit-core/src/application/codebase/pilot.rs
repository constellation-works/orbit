//! Task-pilot receipts and the evidence the Agents › Pilots scorecard grades:
//! each done task a pilot assessed, with its landed diff, the pilot's and the
//! filer's selectors and the paths its run was contested on, and the lock
//! waits other tasks spent on paths each such task held.
//!
//! A task's pilot selectors are the newest task-pilot receipt's
//! `context_files_after`; the filer's are the oldest receipt's
//! `context_files_before`, what the task declared before any pilot ran. The
//! receipt names its pilot job run (`pilot_run_id`), whose record holds the
//! crew and model the run resolved. A receipt written before that field is
//! matched to the one task-pilot run active when it was written, or among
//! several, the one whose step outputs carry its operation id.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::JobRunQuery;
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
/// record resolved (either `None` when it recorded none) and how long it ran.
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
}

/// The workspace's task-pilot runs, read once per scan without their steps.
struct PilotRuns {
    runs: Vec<RunRecord>,
}

impl PilotRuns {
    fn load(runtime: &OrbitRuntime) -> Self {
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
            })
            .into_iter()
            .map(|run| RunRecord {
                started_at: run.started_at,
                finished_at: run.finished_at,
                run: PilotRun {
                    run_id: run.run_id.to_string(),
                    crew: run.resolved_crew,
                    model: run.crew_model,
                    seconds: run.started_at.zip(run.finished_at).map(|(start, end)| {
                        (end - start).num_milliseconds().max(0) as f64 / 1000.0
                    }),
                },
            })
            .collect();
        Self { runs }
    }

    /// The run a receipt names, else the one task-pilot run active when it
    /// was written, else among several the one whose step outputs carry its
    /// operation id.
    fn attribute(
        &self,
        runtime: &OrbitRuntime,
        run_id: Option<&str>,
        at: DateTime<Utc>,
        operation_id: &str,
        now: DateTime<Utc>,
    ) -> Option<PilotRun> {
        if let Some(run_id) = run_id {
            return self
                .runs
                .iter()
                .find(|record| record.run.run_id == run_id)
                .map(|record| record.run.clone());
        }
        let active: Vec<&RunRecord> = self
            .runs
            .iter()
            .filter(|record| {
                record.started_at.is_some_and(|started| started <= at)
                    && record.finished_at.unwrap_or(now) >= at
            })
            .collect();
        let found: Vec<&RunRecord> = if active.len() > 1 {
            active
                .into_iter()
                .filter(|record| {
                    runtime
                        .read_run_state(&record.run.run_id)
                        .ok()
                        .flatten()
                        .and_then(|state| serde_json::to_string(&state.step_outputs).ok())
                        .is_some_and(|outputs| outputs.contains(operation_id))
                })
                .collect()
        } else {
            active
        };
        match found.as_slice() {
            [record] => Some(record.run.clone()),
            _ => None,
        }
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
/// wait, whenever it happened, on a path one of them held.
pub(in crate::application) fn pilot_evidence(
    runtime: &OrbitRuntime,
    facts: &CodebaseFacts,
) -> PilotEvidence {
    let mut runs = None;
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
