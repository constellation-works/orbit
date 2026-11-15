//! Live facts a condition reads, gathered once per pass or explain. Every
//! read here is observation: the task store and task history, the drain's
//! eligibility view without its commit boundary, and the job store. Nothing
//! is written.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use orbit_automation::condition::{
    AcceptedHistoryFacts, BoardFacts, ConditionFacts, ConditionReads, OriginFacts, SelfFacts,
    TaskFacts, TaskPrefilter,
};
use orbit_common::OrbitError;
use orbit_store::contracts::JobRunQuery;
use orbit_types::task::{Task, TaskHistoryEntry, TaskStatus};
use orbit_types::workflow::automation::{CONDITION_FIRED_EVENT, ConditionOrigin};
use orbit_types::workflow::{AutoTaskAcceptedHistory, AutoTaskDefinition, JobRunState};
use serde::Serialize;

use super::super::AutomationBoardPort;
use crate::OrbitRuntime;
use crate::application::automation::AutomationAutoTaskPort;

/// The board-independent inputs: the definition's own minted tasks, which
/// `board` leaves out, and its `self` history.
#[derive(Debug, Clone, Default)]
pub(crate) struct FactInputs {
    pub(crate) own_tasks: BTreeSet<String>,
    pub(crate) self_facts: SelfFacts,
}

/// `self` with the definition's accepted-result history added. Only an
/// auto-task that declares a completion contract has such a history; any other
/// definition keeps `self` as its fire records give it. Missing history leaves
/// `last_succeeded_at` absent; unreadable history is recorded apart, so a read
/// of the field is unknown rather than "never succeeded".
pub(crate) fn with_accepted_history(
    runtime: &OrbitRuntime,
    definition: Option<&AutoTaskDefinition>,
    mut facts: SelfFacts,
) -> SelfFacts {
    let Some(definition) = definition.filter(|definition| definition.completion.is_some()) else {
        return facts;
    };
    match runtime.auto_task_completion_history(&definition.name) {
        AutoTaskAcceptedHistory::Latest(receipt) => {
            facts.last_succeeded_at = Some(receipt.accepted_at)
        }
        AutoTaskAcceptedHistory::Missing => {}
        AutoTaskAcceptedHistory::Unreadable(why) => facts.last_succeeded_unreadable = Some(why),
    }
    facts
}

/// Why `self.last_succeeded_at` is unavailable, in the form the `unavailable`
/// list carries.
pub(crate) fn unreadable_history_note(facts: &SelfFacts) -> Option<String> {
    facts
        .last_succeeded_unreadable
        .as_ref()
        .map(|why| format!("self.last_succeeded_at: {why}"))
}

/// The accepted-result history of each auto-task `names` lists, as
/// `auto_tasks['<name>']` reads it. Only a definition that exists and declares
/// a completion contract has such a history, and only a result that contract
/// accepted moves it: a task that merely finished, a fire or a mint never
/// does. A missing definition, a missing contract and an unreadable history
/// are unavailable, never "never succeeded"; a readable history with no
/// accepted result leaves `last_succeeded_at` absent.
pub(crate) fn named_accepted_history(
    runtime: &OrbitRuntime,
    names: &BTreeSet<String>,
) -> BTreeMap<String, AcceptedHistoryFacts> {
    names
        .iter()
        .map(|name| (name.clone(), accepted_history_of(runtime, name)))
        .collect()
}

fn accepted_history_of(runtime: &OrbitRuntime, name: &str) -> AcceptedHistoryFacts {
    let unavailable = |why: String| AcceptedHistoryFacts {
        last_succeeded_at: None,
        unavailable: Some(why),
    };
    let definition = match runtime.validated_auto_task_definition(name) {
        Ok(Some(definition)) => definition,
        Ok(None) => return unavailable(format!("no auto-task `{name}` is defined here")),
        Err(error) => return unavailable(format!("auto-task `{name}` does not load: {error}")),
    };
    if definition.completion.is_none() {
        return unavailable(format!(
            "auto-task `{name}` declares no completion contract, so it has no accepted results"
        ));
    }
    match runtime.auto_task_completion_history(name) {
        AutoTaskAcceptedHistory::Latest(receipt) => AcceptedHistoryFacts {
            last_succeeded_at: Some(receipt.accepted_at),
            unavailable: None,
        },
        AutoTaskAcceptedHistory::Missing => AcceptedHistoryFacts::default(),
        AutoTaskAcceptedHistory::Unreadable(why) => unavailable(why),
    }
}

/// Why each unavailable `auto_tasks['<name>']` history could not be read, in
/// the form the `unavailable` list carries.
pub(crate) fn unavailable_history_notes(
    histories: &BTreeMap<String, AcceptedHistoryFacts>,
) -> impl Iterator<Item = String> + '_ {
    histories.iter().filter_map(|(name, history)| {
        history
            .unavailable
            .as_ref()
            .map(|why| format!("auto_tasks['{name}'].last_succeeded_at: {why}"))
    })
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
    let auto_tasks = named_accepted_history(runtime, &reads.auto_task_histories);
    unavailable.extend(unavailable_history_notes(&auto_tasks));
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
            auto_tasks,
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
    orbit_common::protocol::serde_string_label(value).unwrap_or_default()
}
