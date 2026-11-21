//! The evidence the Agents › Pilots scorecard grades: assessed done tasks
//! and the lock waits on paths they held.

use std::collections::{BTreeSet, HashMap};

use chrono::{DateTime, Utc};

use super::super::facts::{Anchor, BlockCause, CodebaseFacts};
use super::super::lock_waits::wait_episodes;
use super::super::regression::RegressionFilter;
use super::super::sections::{LARGE_FOOTPRINT_CAP, Params, Rollup};
use super::{PilotRun, PilotRuns};
use crate::OrbitRuntime;

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
    // Denials by the task they named, in recorded order, so each assessed
    // task reads its own instead of scanning every denial.
    let mut denials_by_task: HashMap<&str, Vec<usize>> = HashMap::new();
    for (index, denial) in facts.lock_denials.iter().enumerate() {
        for task_id in &denial.task_ids {
            denials_by_task
                .entry(task_id.as_str())
                .or_default()
                .push(index);
        }
    }
    let mut assessments = Vec::new();
    for task in &facts.tasks {
        let (Some(landed), Some(inputs)) = (&task.landed, &task.pilot) else {
            continue;
        };
        if task.no_diff || landed.paths.len() > LARGE_FOOTPRINT_CAP {
            continue;
        }
        let runs = runs.get_or_insert_with(|| PilotRuns::load(runtime));
        runs.prefetch_attribution_inputs(runtime);
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
        for denial in denials_by_task
            .get(task.id.as_str())
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .map(|&index| &facts.lock_denials[index])
        {
            contested.extend(denial.files.iter().map(|path| Contest {
                at: denial.at,
                path: path.clone(),
                kind: "lock conflict",
            }));
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
