//! Workspace scans and the envelope facts the rankings slice by window.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::StoreRetentionBackend;
use orbit_types::task::{
    NO_DIFF_EXPECTED_TAG, RegressionRiskMarks, Task, TaskHistoryEntry, TaskRelationType, TaskStatus,
};

use super::super::regression::{PendingLink, RegressionLink};
use super::block_cause::failed_claim_settlements;
use super::lock_denial::{lock_denial, requester_identities};
use super::types::{Anchor, Block};
use crate::OrbitRuntime;

#[derive(Debug, Clone)]
pub(in crate::application::codebase) struct TaskFacts {
    pub(in crate::application::codebase) landed: Option<super::super::landed::Landed>,
    /// Landing receipt time, else done time: admits delivery fallback and
    /// places unavailable deliveries in the same window.
    pub(in crate::application::codebase) delivery_at: Option<DateTime<Utc>>,
    /// For a landed task, what its task-pilot receipts record; attributed to
    /// a pilot run only when the pilots scorecard reads it.
    pub(in crate::application::codebase) pilot: Option<super::super::pilot::PilotInputs>,
    /// Tagged `no-diff-expected`: the task was not meant to change files.
    pub(in crate::application::codebase) no_diff: bool,
    pub(in crate::application::codebase) id: String,
    pub(in crate::application::codebase) title: String,
    pub(in crate::application::codebase) crew: Option<String>,
    /// The assessed complexity, as its tier name.
    pub(in crate::application::codebase) complexity: Option<String>,
    pub(in crate::application::codebase) status: TaskStatus,
    /// The declared footprint: `context_files`, widened selectors included.
    pub(in crate::application::codebase) footprint: Vec<Anchor>,
    /// Last update; places the task in a window for denominators.
    pub(in crate::application::codebase) updated_at: DateTime<Utc>,
    /// When the task last became `done`, while it is done.
    pub(in crate::application::codebase) done_at: Option<DateTime<Utc>>,
    /// When the task last became `rejected`, while it is rejected.
    pub(in crate::application::codebase) rejected_at: Option<DateTime<Utc>>,
    /// Whether any job run ever picked the task up.
    pub(in crate::application::codebase) had_run: bool,
    /// Each `context_files_widened` event: when, and the paths it added.
    pub(in crate::application::codebase) widenings: Vec<(DateTime<Utc>, Vec<Anchor>)>,
    /// Each entry into `blocked`, with its classified cause.
    pub(in crate::application::codebase) blocks: Vec<Block>,
    /// The `regression_from` links that target this task, one per
    /// regression task, by its ID.
    pub(in crate::application::codebase) regressions: Vec<super::super::regression::RegressionLink>,
    /// When the task entered a status other than `backlog`, ascending: each
    /// ends any lock wait in progress, since only a backlog task is admitted.
    pub(in crate::application::codebase) left_backlog: Vec<DateTime<Utc>>,
    /// Each `regression_risk_marked` event: when a prompt for the task
    /// marked context files, and what it marked.
    pub(in crate::application::codebase) marks: Vec<(DateTime<Utc>, RegressionRiskMarks)>,
}

impl TaskFacts {
    /// A task's identity, status and declared footprint, with no history
    /// read: the facts a [`scan_regressions`] task carries.
    fn from_envelope(task: &Task, done_at: Option<DateTime<Utc>>) -> Self {
        let mut footprint: Vec<Anchor> = task
            .context_files
            .iter()
            .filter_map(|selector| Anchor::parse(selector))
            .collect();
        footprint.sort();
        footprint.dedup();
        Self {
            landed: None,
            delivery_at: done_at,
            pilot: None,
            no_diff: task.tags.iter().any(|tag| tag == NO_DIFF_EXPECTED_TAG),
            id: task.id.to_string(),
            title: task.title.clone(),
            crew: task.crew.clone().or_else(|| task.implemented_by.clone()),
            complexity: task
                .complexity
                .map(|complexity| complexity.as_str().to_string()),
            status: task.status,
            footprint,
            updated_at: task.updated_at,
            done_at,
            rejected_at: None,
            had_run: false,
            widenings: Vec::new(),
            blocks: Vec::new(),
            regressions: Vec::new(),
            left_backlog: Vec::new(),
            marks: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub(in crate::application::codebase) struct ReservationFacts {
    pub(in crate::application::codebase) at: DateTime<Utc>,
    /// Each requested task, or the owning run for a file-only reservation.
    /// Without either, the reservation itself identifies one request.
    pub(in crate::application::codebase) requesters: Vec<String>,
    pub(in crate::application::codebase) files: Vec<Anchor>,
}

#[derive(Debug, Clone)]
pub(in crate::application::codebase) struct LockDenial {
    pub(in crate::application::codebase) at: DateTime<Utc>,
    /// Who waited: each requested task, or the run (or invocation) when the
    /// request named no task. A retry loop repeats the same contender.
    pub(in crate::application::codebase) contenders: Vec<String>,
    /// The tasks the request named; empty for a file-only request. Only a
    /// task's wait has an admission to end it.
    pub(in crate::application::codebase) task_ids: Vec<String>,
    /// The files the request conflicted on.
    pub(in crate::application::codebase) files: Vec<Anchor>,
    /// Each conflicting file and a task holding it: the task whose context
    /// lock held it, or each task the holding reservation named.
    pub(in crate::application::codebase) holders: Vec<(Anchor, String)>,
}

/// One workspace scan: the per-task facts every ranking slices by window,
/// minimum support and rollup.
#[derive(Debug)]
pub struct CodebaseFacts {
    pub(in crate::application::codebase) computed_at: DateTime<Utc>,
    pub(in crate::application::codebase) tasks: Vec<TaskFacts>,
    pub(in crate::application::codebase) reservations: Vec<ReservationFacts>,
    pub(in crate::application::codebase) lock_denials: Vec<LockDenial>,
    /// When each task was granted a reservation, ascending: its admission.
    pub(in crate::application::codebase) admissions: BTreeMap<String, Vec<DateTime<Utc>>>,
}

/// Scan the workspace once. A task whose history cannot be read keeps its
/// envelope facts with an empty history rather than failing the whole view.
/// Each task's [`super::history::HistoryFacts`] are reused from the store while its history
/// stamp and settlements match, and computed and stored otherwise.
pub(in crate::application::codebase) fn scan(
    runtime: &OrbitRuntime,
    delivery_window: Option<&super::super::sections::Params>,
    include_landed: bool,
) -> Result<CodebaseFacts, OrbitError> {
    let computed_at = Utc::now();
    let tasks = runtime.list_task_metadata()?;
    let pending_links = pending_links(&tasks);

    let settlements = failed_claim_settlements(runtime);
    let mut stored = super::super::stored::StoredFacts::load(runtime);
    let mut facts = Vec::with_capacity(tasks.len());
    let mut landed_scan = include_landed.then(|| super::super::landed::Scanner::new(runtime));
    for task in tasks {
        let id = task.id.to_string();
        let task_settlements = settlements.get(&id).map(Vec::as_slice).unwrap_or_default();
        let mut entry = stored.entry(runtime, &id, task_settlements);
        let done_at = (task.status == TaskStatus::Done)
            .then(|| entry.facts.entered_done.unwrap_or(task.updated_at));
        let rejected_at = (task.status == TaskStatus::Rejected)
            .then(|| entry.facts.entered_rejected.unwrap_or(task.updated_at));
        let had_run =
            task.job_run_id.is_some() || !entry.facts.widenings.is_empty() || entry.facts.marks_run;
        let mut footprint: Vec<Anchor> = task
            .context_files
            .iter()
            .filter_map(|selector| Anchor::parse(selector))
            .collect();
        footprint.sort();
        footprint.dedup();
        let delivery_at = done_at.map(|at| {
            entry
                .facts
                .receipt
                .as_ref()
                .map_or(at, |(_, landed_at)| *landed_at)
        });
        let landed = delivery_at
            .filter(|at| delivery_window.is_none_or(|params| params.within(*at)))
            .and_then(|_| {
                let scanner = landed_scan.as_mut()?;
                let Some((commit, at)) = entry.facts.receipt.clone() else {
                    return delivery_window
                        .is_some()
                        .then(|| scanner.observe(runtime, &id))
                        .flatten();
                };
                if entry.facts.landed_paths.is_none() {
                    entry.facts.landed_paths = scanner.changed_paths(&commit);
                    entry.dirty |= entry.facts.landed_paths.is_some();
                }
                Some(super::super::landed::Landed {
                    at,
                    commit,
                    paths: entry.facts.landed_paths.clone()?,
                })
            });
        let no_diff = task.tags.iter().any(|tag| tag == NO_DIFF_EXPECTED_TAG);
        let pilot = (landed.is_some() && !no_diff)
            .then(|| {
                // A delivery-observed landing has no receipt, so its pilot
                // receipts were not read with its stored facts.
                if entry.facts.receipt.is_some() {
                    entry.facts.pilot.clone()
                } else {
                    super::super::pilot::inputs(runtime, &id).ok().flatten()
                }
            })
            .flatten();
        stored.keep(&id, &entry);
        let history = entry.facts;
        facts.push(TaskFacts {
            landed,
            delivery_at,
            pilot,
            no_diff,
            regressions: Vec::new(),
            id,
            title: task.title,
            crew: task.crew.or(task.implemented_by),
            complexity: task
                .complexity
                .map(|complexity| complexity.as_str().to_string()),
            status: task.status,
            footprint,
            updated_at: task.updated_at,
            done_at,
            rejected_at,
            had_run,
            widenings: history.widenings,
            blocks: history.blocks,
            left_backlog: history.left_backlog,
            marks: history.marks,
        });
    }
    stored.save(runtime, facts.iter().map(|task| task.id.as_str()));
    attach_regressions(&mut facts, pending_links);

    let mut admissions: BTreeMap<String, Vec<DateTime<Utc>>> = BTreeMap::new();
    let mut reservation_tasks: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let reservations = runtime
        .list_task_reservation_history()?
        .into_iter()
        .filter_map(|record| {
            let at = DateTime::parse_from_rfc3339(&record.created_at)
                .ok()?
                .with_timezone(&Utc);
            for task_id in &record.task_ids {
                admissions.entry(task_id.clone()).or_default().push(at);
            }
            reservation_tasks.insert(record.reservation_id.clone(), record.task_ids.clone());
            Some(ReservationFacts {
                at,
                requesters: requester_identities(
                    record.task_ids,
                    record.owner_run_id,
                    format!("reservation:{}", record.reservation_id),
                ),
                files: record
                    .files
                    .iter()
                    .filter_map(|file| Anchor::parse(file))
                    .collect(),
            })
        })
        .collect();

    // Legacy unscoped denials cannot be attributed safely. The durable
    // workspace-scoped facts survive command-audit retention.
    let lock_denials = runtime
        .sqlite_store()?
        .list_task_lock_denials(&runtime.workspace_id()?)?
        .iter()
        .filter_map(|event| lock_denial(event, &reservation_tasks))
        .collect();

    for grants in admissions.values_mut() {
        grants.sort();
    }

    Ok(CodebaseFacts {
        computed_at,
        tasks: facts,
        reservations,
        lock_denials,
        admissions,
    })
}

/// The facts the regression-risk marks read, and nothing else: every task's
/// footprint and incoming `regression_from` links, with history read only
/// for a done task that may have become done since `since`. A task updated
/// before then became done before then too, so its update time stands in.
pub(in crate::application::codebase) fn scan_regressions(
    runtime: &OrbitRuntime,
    since: DateTime<Utc>,
) -> Result<CodebaseFacts, OrbitError> {
    let computed_at = Utc::now();
    let tasks = runtime.list_task_metadata()?;
    let pending_links = pending_links(&tasks);
    let mut facts: Vec<TaskFacts> = tasks
        .iter()
        .map(|task| {
            let done_at = (task.status == TaskStatus::Done).then(|| {
                if task.updated_at < since {
                    return task.updated_at;
                }
                let id = task.id.to_string();
                let history = runtime.get_task_history(&id).unwrap_or_else(|error| {
                    tracing::warn!(task_id = %id, %error, "regression risk: task history unreadable");
                    Vec::new()
                });
                entered(&history, TaskStatus::Done).unwrap_or(task.updated_at)
            });
            TaskFacts::from_envelope(task, done_at)
        })
        .collect();
    attach_regressions(&mut facts, pending_links);
    Ok(CodebaseFacts {
        computed_at,
        tasks: facts,
        reservations: Vec::new(),
        lock_denials: Vec::new(),
        admissions: BTreeMap::new(),
    })
}

/// When the task last entered `status`, by its history.
pub(super) fn entered(history: &[TaskHistoryEntry], status: TaskStatus) -> Option<DateTime<Utc>> {
    history
        .iter()
        .rev()
        .find(|entry| entry.to_status == Some(status))
        .map(|entry| entry.at)
}

/// Every `regression_from` link, as its regression task records it.
fn pending_links(tasks: &[Task]) -> Vec<PendingLink> {
    let mut pending = Vec::new();
    for task in tasks {
        for relation in &task.relations {
            if relation.relation_type == TaskRelationType::RegressionFrom {
                pending.push(PendingLink::new(
                    relation.target.to_string(),
                    task,
                    relation.regression,
                ));
            }
        }
    }
    pending
}

/// Give each regressed task its links. A link's layer reads its regression
/// task's landed paths, known only once every task is scanned.
fn attach_regressions(facts: &mut [TaskFacts], pending_links: Vec<PendingLink>) {
    let index: BTreeMap<String, usize> = facts
        .iter()
        .enumerate()
        .map(|(at, task)| (task.id.clone(), at))
        .collect();
    let mut links: BTreeMap<String, Vec<RegressionLink>> = BTreeMap::new();
    for pending in pending_links {
        let fix = index.get(&pending.task_id).map(|at| &facts[*at]);
        let regressed = pending.regressed.clone();
        links
            .entry(regressed)
            .or_default()
            .push(pending.resolve(fix));
    }
    for (regressed, mut task_links) in links {
        if let Some(at) = index.get(&regressed) {
            task_links.sort_by(|left, right| left.task_id.cmp(&right.task_id));
            facts[*at].regressions = task_links;
        }
    }
}
