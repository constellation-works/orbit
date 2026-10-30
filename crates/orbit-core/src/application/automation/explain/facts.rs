//! Live facts a condition reads, gathered once per explain. Every read here
//! is observation: the task store, the drain's eligibility view without its
//! commit boundary, and the job store. Nothing is written.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use orbit_automation::condition::{
    BoardFacts, CheckedCondition, ConditionFacts, SelfFacts, TaskFacts, TaskPrefilter,
};
use orbit_common::OrbitError;
use orbit_store::contracts::JobRunQuery;
use orbit_types::task::{Task, TaskStatus};
use orbit_types::workflow::JobRunState;
use orbit_types::workflow::automation::ConditionScope;
use serde::Serialize;

use super::super::AutomationBoardPort;
use crate::OrbitRuntime;

/// Most subjects one explain evaluates, the per-pass limit of §4.
pub(super) const MAX_EXPLAIN_SUBJECTS: usize = 200;

/// The facts plus the subject set they were gathered for.
pub(super) struct GatheredFacts {
    pub(super) facts: ConditionFacts,
    /// Task scope: the prefiltered tasks, in ID order, at most
    /// [`MAX_EXPLAIN_SUBJECTS`]. Workspace scope: empty.
    pub(super) subjects: Vec<TaskFacts>,
    /// How many tasks the prefilter selected before the cap.
    pub(super) selected: usize,
    /// Board values that could not be read, with why.
    pub(super) unavailable: Vec<String>,
    /// Every task the workspace lists, by ID, for a `--subject` outside the
    /// prefilter.
    pub(super) tasks: BTreeMap<String, Task>,
}

pub(super) fn gather(
    runtime: &OrbitRuntime,
    condition: &CheckedCondition,
    now: DateTime<Utc>,
) -> Result<GatheredFacts, OrbitError> {
    let tasks = runtime
        .list_task_metadata()?
        .into_iter()
        .map(|task| (task.id.clone(), task))
        .collect::<BTreeMap<_, _>>();
    let mut unavailable = Vec::new();
    let board = board_facts(runtime, condition, &tasks, &mut unavailable);
    let mut cross_reads = BTreeMap::new();
    for id in &condition.reads.task_ids {
        if let Some(task) = tasks.get(id) {
            cross_reads.insert(
                id.clone(),
                task_facts(runtime, task, condition.reads.status_since)?,
            );
        }
    }
    let (subjects, selected) = match condition.scope {
        ConditionScope::Task => {
            let mut selected = tasks
                .values()
                .filter(|task| passes_prefilter(task, &condition.prefilter))
                .collect::<Vec<_>>();
            selected.sort_by(|left, right| natural_id_order(&left.id, &right.id));
            let count = selected.len();
            let subjects = selected
                .into_iter()
                .take(MAX_EXPLAIN_SUBJECTS)
                .map(|task| task_facts(runtime, task, condition.reads.status_since))
                .collect::<Result<Vec<_>, _>>()?;
            (subjects, count)
        }
        _ => (Vec::new(), 1),
    };
    Ok(GatheredFacts {
        facts: ConditionFacts {
            now,
            board,
            tasks: cross_reads,
            // Fire records ship with firing; until then a definition has none.
            self_facts: SelfFacts::default(),
        },
        subjects,
        selected,
        unavailable,
        tasks,
    })
}

/// IDs compare by prefix, then by their numeric suffix (`ORB-9` < `ORB-10`).
pub(super) fn natural_id_order(left: &str, right: &str) -> std::cmp::Ordering {
    let split = |id: &str| {
        let digits = id.len() - id.trim_end_matches(|c: char| c.is_ascii_digit()).len();
        let (prefix, number) = id.split_at(id.len() - digits);
        (
            prefix.to_string(),
            number.parse::<u64>().unwrap_or(0),
            id.to_string(),
        )
    };
    split(left).cmp(&split(right))
}

pub(super) fn passes_prefilter(task: &Task, prefilter: &TaskPrefilter) -> bool {
    let status = serde_name(&task.status);
    (prefilter.statuses.is_empty() || prefilter.statuses.contains(&status))
        && prefilter.tags.iter().all(|tag| task.tags.contains(tag))
}

pub(super) fn task_facts(
    runtime: &OrbitRuntime,
    task: &Task,
    reads_status_since: bool,
) -> Result<TaskFacts, OrbitError> {
    // `status_since` needs the task's history; read it only when the
    // expression does.
    let status_since = if reads_status_since {
        runtime
            .get_task_history(&task.id)?
            .iter()
            .rev()
            .find(|entry| entry.to_status == Some(task.status))
            .map_or(task.created_at, |entry| entry.at)
    } else {
        task.created_at
    };
    Ok(TaskFacts {
        id: task.id.clone(),
        title: task.title.clone(),
        status: serde_name(&task.status),
        tags: task.tags.clone(),
        task_type: serde_name(&task.task_type),
        priority: serde_name(&task.priority),
        complexity: task.complexity.map_or_else(
            || "unassessed".to_string(),
            |complexity| serde_name(&complexity),
        ),
        crew: task.crew.clone(),
        parent_id: task.parent_id().map(str::to_owned),
        dependencies: task.dependencies(),
        created_at: task.created_at,
        updated_at: task.updated_at,
        status_since,
        // Only a condition fire stamps an origin, and nothing fires yet.
        origin: None,
    })
}

fn board_facts(
    runtime: &OrbitRuntime,
    condition: &CheckedCondition,
    tasks: &BTreeMap<String, Task>,
    unavailable: &mut Vec<String>,
) -> BoardFacts {
    let mut by_status = TaskStatus::ALL
        .iter()
        .map(|status| (serde_name(status), 0_i64))
        .collect::<BTreeMap<_, _>>();
    for task in tasks.values() {
        *by_status.entry(serde_name(&task.status)).or_default() += 1;
    }
    let by_tag = condition
        .reads
        .tags
        .iter()
        .map(|tag| {
            let count = tasks
                .values()
                .filter(|task| task.tags.contains(tag))
                .count();
            (tag.clone(), i64::try_from(count).unwrap_or(i64::MAX))
        })
        .collect();
    let count_of = |status: TaskStatus| by_status.get(&serde_name(&status)).copied();
    let reads = &condition.reads.board;
    let eligible = if reads.contains("eligible") {
        match eligible_count(runtime) {
            Ok(count) => Some(count),
            Err(error) => {
                unavailable.push(format!("board.eligible: {error}"));
                None
            }
        }
    } else {
        None
    };
    let (open_runs, drain_running) =
        if reads.contains("open_runs") || reads.contains("drain_running") {
            match open_runs(runtime) {
                Ok((count, drain)) => (Some(count), Some(drain)),
                Err(error) => {
                    unavailable.push(format!("board.open_runs and board.drain_running: {error}"));
                    (None, None)
                }
            }
        } else {
            (None, None)
        };
    BoardFacts {
        eligible,
        backlog: count_of(TaskStatus::Backlog),
        in_progress: count_of(TaskStatus::InProgress),
        blocked: count_of(TaskStatus::Blocked),
        review: count_of(TaskStatus::Review),
        open_runs,
        drain_running,
        by_status,
        by_tag,
    }
}

fn eligible_count(runtime: &OrbitRuntime) -> Result<i64, OrbitError> {
    Ok(i64::try_from(runtime.eligible_task_count()?).unwrap_or(i64::MAX))
}

/// Pending, running and retrying job runs, and whether the local drain is
/// running.
fn open_runs(runtime: &OrbitRuntime) -> Result<(i64, bool), OrbitError> {
    let runs = runtime
        .stores()
        .jobs()
        .list_job_runs_filtered(&JobRunQuery {
            states: vec![
                JobRunState::Pending,
                JobRunState::Running,
                JobRunState::Retrying,
            ],
            ..JobRunQuery::default()
        })?;
    Ok((
        i64::try_from(runs.len()).unwrap_or(i64::MAX),
        runtime.local_drain_running()?,
    ))
}

/// A value's persisted spelling, as the condition schema reads it.
pub(super) fn serde_name<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}
