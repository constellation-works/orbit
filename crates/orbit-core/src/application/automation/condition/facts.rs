//! Live facts a condition reads, gathered once per pass or explain. Every
//! read here is observation: the task store and task history, the drain's
//! eligibility view without its commit boundary, and the job store. Nothing
//! is written.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use orbit_automation::condition::{
    BoardFacts, ConditionFacts, ConditionReads, OriginFacts, SelfFacts, TaskFacts, TaskPrefilter,
};
use orbit_common::OrbitError;
use orbit_store::contracts::JobRunQuery;
use orbit_types::task::{Task, TaskHistoryEntry, TaskStatus};
use orbit_types::workflow::JobRunState;
use orbit_types::workflow::automation::{CONDITION_FIRED_EVENT, ConditionOrigin};
use serde::Serialize;

use super::super::AutomationBoardPort;
use crate::OrbitRuntime;

/// The board-independent inputs: the definition's own minted tasks, which
/// `board` leaves out, and its `self` history.
#[derive(Debug, Clone, Default)]
pub(crate) struct FactInputs {
    pub(crate) own_tasks: BTreeSet<String>,
    pub(crate) self_facts: SelfFacts,
}

/// The facts plus every task the workspace lists.
pub(crate) struct GatheredFacts {
    pub(crate) facts: ConditionFacts,
    /// Board values that could not be read, with why.
    pub(crate) unavailable: Vec<String>,
    /// Every task the workspace lists, by ID.
    pub(crate) tasks: BTreeMap<String, Task>,
}

/// What a task's history says: its position (the observation revision), its
/// creation origin, and when it last moved into its current status.
#[derive(Debug, Clone)]
pub(crate) struct HistoryFacts {
    pub(crate) revision: usize,
    pub(crate) origin: Option<ConditionOrigin>,
    pub(crate) status_since: DateTime<Utc>,
}

pub(crate) fn gather(
    runtime: &OrbitRuntime,
    reads: &ConditionReads,
    now: DateTime<Utc>,
    inputs: &FactInputs,
) -> Result<GatheredFacts, OrbitError> {
    let tasks = runtime
        .list_task_metadata()?
        .into_iter()
        .map(|task| (task.id.clone(), task))
        .collect::<BTreeMap<_, _>>();
    let mut unavailable = Vec::new();
    let board = board_facts(runtime, reads, &tasks, &inputs.own_tasks, &mut unavailable);
    let mut cross_reads = BTreeMap::new();
    for id in &reads.task_ids {
        if let Some(task) = tasks.get(id) {
            cross_reads.insert(id.clone(), task_facts(runtime, task, reads)?.0);
        }
    }
    Ok(GatheredFacts {
        facts: ConditionFacts {
            now,
            board,
            tasks: cross_reads,
            self_facts: inputs.self_facts.clone(),
        },
        unavailable,
        tasks,
    })
}

/// The tasks a task-scope prefilter selects, in natural ID order.
pub(crate) fn prefiltered<'a>(
    tasks: &'a BTreeMap<String, Task>,
    prefilter: &TaskPrefilter,
) -> Vec<&'a Task> {
    let mut selected = tasks
        .values()
        .filter(|task| passes_prefilter(task, prefilter))
        .collect::<Vec<_>>();
    selected.sort_by(|left, right| natural_id_order(&left.id, &right.id));
    selected
}

/// IDs compare by prefix, then by their numeric suffix (`ORB-9` < `ORB-10`).
pub(crate) fn natural_id_order(left: &str, right: &str) -> std::cmp::Ordering {
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

pub(crate) fn passes_prefilter(task: &Task, prefilter: &TaskPrefilter) -> bool {
    let status = serde_name(&task.status);
    (prefilter.statuses.is_empty() || prefilter.statuses.contains(&status))
        && prefilter.tags.iter().all(|tag| task.tags.contains(tag))
}

/// The trusted creation origin in a task's history: a `condition_fired`
/// entry the system wrote whose note is an origin record. Anything else, a
/// caller-written note included, is not an origin.
pub(crate) fn origin_of(history: &[TaskHistoryEntry]) -> Option<ConditionOrigin> {
    history
        .iter()
        .filter(|entry| {
            entry.event == CONDITION_FIRED_EVENT
                && entry.by == crate::application::task::SYSTEM_ACTOR_LABEL
        })
        .find_map(|entry| entry.note.as_deref().and_then(ConditionOrigin::from_note))
}

pub(crate) fn history_facts(
    runtime: &OrbitRuntime,
    task: &Task,
) -> Result<HistoryFacts, OrbitError> {
    let history = runtime.get_task_history(&task.id)?;
    let status_since = history
        .iter()
        .rev()
        .find(|entry| entry.to_status == Some(task.status))
        .map_or(task.created_at, |entry| entry.at);
    Ok(HistoryFacts {
        revision: history.len(),
        origin: origin_of(&history),
        status_since,
    })
}

/// One task's readable fields, reading its history only when the expression
/// reads `status_since` or `origin`; the history facts come back when read.
pub(crate) fn task_facts(
    runtime: &OrbitRuntime,
    task: &Task,
    reads: &ConditionReads,
) -> Result<(TaskFacts, Option<HistoryFacts>), OrbitError> {
    let history = if reads.status_since || reads.origin {
        Some(history_facts(runtime, task)?)
    } else {
        None
    };
    Ok((task_facts_with(task, history.as_ref()), history))
}

pub(crate) fn task_facts_with(task: &Task, history: Option<&HistoryFacts>) -> TaskFacts {
    TaskFacts {
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
        status_since: history.map_or(task.created_at, |history| history.status_since),
        origin: history
            .and_then(|history| history.origin.as_ref())
            .map(|origin| OriginFacts {
                trigger: origin.trigger.clone(),
                fire_id: origin.fire_id.clone(),
                depth: i64::from(origin.depth),
            }),
    }
}

fn board_facts(
    runtime: &OrbitRuntime,
    reads: &ConditionReads,
    tasks: &BTreeMap<String, Task>,
    own_tasks: &BTreeSet<String>,
    unavailable: &mut Vec<String>,
) -> BoardFacts {
    // Tasks this definition minted leave its board (§2), so its own output
    // neither keeps the board busy nor re-arms the subject.
    let counted = tasks
        .values()
        .filter(|task| !own_tasks.contains(&task.id))
        .collect::<Vec<_>>();
    let mut by_status = TaskStatus::ALL
        .iter()
        .map(|status| (serde_name(status), 0_i64))
        .collect::<BTreeMap<_, _>>();
    for task in &counted {
        *by_status.entry(serde_name(&task.status)).or_default() += 1;
    }
    let by_tag = reads
        .tags
        .iter()
        .map(|tag| {
            let count = counted
                .iter()
                .filter(|task| task.tags.contains(tag))
                .count();
            (tag.clone(), i64::try_from(count).unwrap_or(i64::MAX))
        })
        .collect();
    let count_of = |status: TaskStatus| by_status.get(&serde_name(&status)).copied();
    let board = &reads.board;
    let eligible = if board.contains("eligible") {
        match runtime.eligible_task_ids() {
            Ok(ids) => Some(
                i64::try_from(ids.iter().filter(|id| !own_tasks.contains(*id)).count())
                    .unwrap_or(i64::MAX),
            ),
            Err(error) => {
                unavailable.push(format!("board.eligible: {error}"));
                None
            }
        }
    } else {
        None
    };
    let (open_runs, drain_running) =
        if board.contains("open_runs") || board.contains("drain_running") {
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
pub(crate) fn serde_name<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}
