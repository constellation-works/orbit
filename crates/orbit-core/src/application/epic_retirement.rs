//! Epic-retirement safety check [ORB-12491].
//!
//! Epic execution is gone: there is no `epic_pipeline`, no `epic_orchestrator`,
//! no descendant-union footprint, and no epic-shaped admission — an
//! `epic`-tagged root is an ordinary leaf reserving what it declares. The one
//! case admission still withholds is the root that declared nothing while its
//! descendants did, which is [`orbit_types::task::inherited_only_epic_roots`]
//! and is the same population this module reports. What a workspace written by
//! an older binary may still hold is *state*
//! from that machinery — a run that never reached a terminal row, a reservation
//! a drain never released, a landing whose outcome nobody can read off the
//! store. Deleting such a store's epic records, or admitting those tasks as
//! ordinary leaves, would race work that is still live.
//!
//! This module answers one question and performs no write: **may an operator
//! retire the epic records in this workspace now?** The answer is keyed on runs
//! and reservations, never on the root's task status — a root already in
//! `review` can still have a live `complete_pr` step, so a status-only check is
//! insufficient. It also reports the roots whose only context came from their
//! children — which admission no longer inherits, and therefore withholds
//! until an operator repairs them — and the historical runs whose worktrees GC
//! must still be able to find.
//!
//! [`assess_epic_retirement`] is a pure function over a snapshot so the rule is
//! testable without a store; [`OrbitRuntime::epic_retirement_readiness`] is the
//! read-only gatherer that builds that snapshot from the live one.

use std::collections::{BTreeMap, BTreeSet};

use orbit_common::OrbitError;
use orbit_store::contracts::{ActiveTaskReservation, JobRunQuery};
use orbit_types::task::{
    EpicHierarchyNode, Task, TaskStatus, has_epic_tag, inherited_only_epic_roots,
};
use orbit_types::workflow::{JobRun, JobRunState};
use serde::Serialize;
use serde_json::Value;

use crate::OrbitRuntime;
use crate::runtime::task::locks::{workspace_orbit_dir, workspace_task_reservation_id};

/// The retired job. Its stored runs are the migration's primary evidence.
pub const RETIRED_EPIC_JOB_ID: &str = "epic_pipeline";

/// Guard on the parent walk, matching the admission-side chain limit.
const MAX_TASK_PARENT_CHAIN_DEPTH: usize = 32;

/// Why a workspace may not retire its epic records yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EpicRetirementBlockerKind {
    /// A `epic_pipeline` run that never reached a terminal state.
    ActiveEpicRun,
    /// Any other run still carrying a task in an epic family — the child
    /// executions an epic drained sequentially.
    ActiveFamilyRun,
    /// A run that stopped somewhere no one can read off the store (non-success
    /// terminal state), or an active family task whose recorded run is missing.
    /// Its branch, PR, and task status cannot be assumed reconciled.
    UncertainLanding,
    /// An unreleased task reservation naming an epic-family task.
    ActiveReservation,
}

/// One reason migration refuses, with the record that proves it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EpicRetirementBlocker {
    pub kind: EpicRetirementBlockerKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reservation_id: Option<String>,
    pub task_ids: Vec<String>,
    pub detail: String,
}

/// An `epic`-tagged root that declared no context of its own and relied on the
/// union its descendants supplied. Nothing inherits now, so such a root
/// reserves nothing: admission withholds it and `reserve_locks` refuses it
/// (both through [`orbit_types::task::inherited_only_epic_roots`], which
/// decides this population once), and it needs operator-supplied own context
/// or deliberate retirement. Reported here, never rewritten.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InheritedOnlyRoot {
    pub task_id: String,
    pub status: String,
    /// Descendants that do declare context, in task-ID order — the material an
    /// operator repairs the root from.
    pub descendants_with_context: Vec<String>,
}

/// The migration's answer for one workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EpicRetirementReport {
    /// True only when nothing unreconciled remains. Reported roots do not
    /// withhold it: they are an admission repair, not live state.
    pub ready: bool,
    pub blockers: Vec<EpicRetirementBlocker>,
    pub inherited_only_roots: Vec<InheritedOnlyRoot>,
    /// Terminal `epic_pipeline` runs whose stored input still names a worktree.
    /// Worktree decoding for these is retained; verify the sweep reaped them
    /// before removing the historical records they are discovered through.
    pub historical_worktree_runs: Vec<String>,
    /// Every task carrying the `epic` tag, root or not. Tags and hierarchy
    /// survive migration untouched; this is the inventory, not a work list.
    pub epic_tagged_tasks: Vec<String>,
}

/// The state one assessment reads. Supplied whole so the rule is a pure
/// function of it.
#[derive(Debug, Clone, Copy)]
pub struct EpicRetirementSnapshot<'a> {
    pub tasks: &'a [Task],
    /// Every recorded run, terminal or not. Steps are not required: a run that
    /// never finalized is already non-terminal here.
    pub runs: &'a [JobRun],
    pub reservations: &'a [ActiveTaskReservation],
}

/// Decide whether the epic records in `snapshot` may be retired.
///
/// Refuses on any unreconciled epic execution, child execution, reservation, or
/// uncertain landing, regardless of the owning task's status. Performs no write
/// and reads no store.
pub fn assess_epic_retirement(snapshot: EpicRetirementSnapshot<'_>) -> EpicRetirementReport {
    let by_id: BTreeMap<&str, &Task> = snapshot
        .tasks
        .iter()
        .map(|task| (task.id.as_str(), task))
        .collect();
    let family = epic_family(&by_id);
    let mut blockers = Vec::new();
    let mut historical_worktree_runs = Vec::new();

    for run in snapshot.runs {
        let is_epic_run = run.job_id == RETIRED_EPIC_JOB_ID;
        let run_task_ids = run_task_ids(run);
        let carried: Vec<String> = run_task_ids
            .iter()
            .filter(|task_id| family.contains(task_id.as_str()))
            .cloned()
            .collect();
        if !is_epic_run && carried.is_empty() {
            continue;
        }
        let task_ids = if carried.is_empty() {
            run_task_ids.clone()
        } else {
            carried
        };
        if !run.state.is_terminal() {
            blockers.push(EpicRetirementBlocker {
                kind: if is_epic_run {
                    EpicRetirementBlockerKind::ActiveEpicRun
                } else {
                    EpicRetirementBlockerKind::ActiveFamilyRun
                },
                run_id: Some(run.run_id.clone()),
                reservation_id: None,
                task_ids,
                detail: format!(
                    "run `{}` of `{}` is {} and may still be landing; drain it or stop and reconcile it",
                    run.run_id,
                    run.job_id,
                    run.state
                ),
            });
        } else if run.state != JobRunState::Success {
            blockers.push(EpicRetirementBlocker {
                kind: EpicRetirementBlockerKind::UncertainLanding,
                run_id: Some(run.run_id.clone()),
                reservation_id: None,
                task_ids,
                detail: format!(
                    "run `{}` of `{}` ended {}; where its landing stopped is not readable from the store, so reconcile its branch, PR, and task status first",
                    run.run_id,
                    run.job_id,
                    run.state
                ),
            });
        }
        if is_epic_run && run.state.is_terminal() && !run_task_ids.is_empty() {
            historical_worktree_runs.push(run.run_id.clone());
        }
    }

    let recorded_runs: BTreeSet<&str> = snapshot
        .runs
        .iter()
        .map(|run| run.run_id.as_str())
        .collect();
    for task in snapshot.tasks {
        if !family.contains(task.id.as_str())
            || !matches!(task.status, TaskStatus::InProgress | TaskStatus::Review)
        {
            continue;
        }
        let Some(job_run_id) = task
            .job_run_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
        else {
            continue;
        };
        if recorded_runs.contains(job_run_id) {
            continue;
        }
        blockers.push(EpicRetirementBlocker {
            kind: EpicRetirementBlockerKind::UncertainLanding,
            run_id: Some(job_run_id.to_string()),
            reservation_id: None,
            task_ids: vec![task.id.clone()],
            detail: format!(
                "task `{}` is `{}` against run `{job_run_id}`, which this store has no record of; its landing cannot be verified here",
                task.id, task.status
            ),
        });
    }

    for reservation in snapshot.reservations {
        let carried: Vec<String> = reservation
            .task_ids
            .iter()
            .filter(|task_id| family.contains(task_id.as_str()))
            .cloned()
            .collect();
        if carried.is_empty() {
            continue;
        }
        blockers.push(EpicRetirementBlocker {
            kind: EpicRetirementBlockerKind::ActiveReservation,
            run_id: reservation.owner_run_id.clone(),
            reservation_id: Some(reservation.reservation_id.clone()),
            task_ids: carried,
            detail: format!(
                "reservation `{}` still holds {} file(s) for epic-family work; release it before retiring",
                reservation.reservation_id,
                reservation.files.len()
            ),
        });
    }

    blockers.sort_by(|left, right| {
        left.task_ids
            .cmp(&right.task_ids)
            .then(left.run_id.cmp(&right.run_id))
            .then(left.reservation_id.cmp(&right.reservation_id))
    });
    historical_worktree_runs.sort();
    historical_worktree_runs.dedup();

    EpicRetirementReport {
        ready: blockers.is_empty(),
        blockers,
        inherited_only_roots: inherited_only_roots(&by_id),
        historical_worktree_runs,
        epic_tagged_tasks: snapshot
            .tasks
            .iter()
            .filter(|task| is_epic_tagged(task))
            .map(|task| task.id.clone())
            .collect(),
    }
}

impl OrbitRuntime {
    /// [`assess_epic_retirement`] over this workspace's live records.
    ///
    /// Read-only: it lists tasks, runs, and active reservations and decides.
    /// Nothing here deletes an epic record, rewrites a task, or reconciles a
    /// run — the refusal is the product.
    pub fn epic_retirement_readiness(&self) -> Result<EpicRetirementReport, OrbitError> {
        let tasks = self.stores().tasks().list_tasks()?;
        let runs = self.stores().jobs().list_job_runs_filtered(&JobRunQuery {
            include_steps: false,
            ..JobRunQuery::default()
        })?;
        let workspace_id = workspace_task_reservation_id(self)?;
        let reservations = self
            .stores()
            .task_reservations()
            .list_active_task_reservations(&workspace_orbit_dir(self), workspace_id.as_deref())?
            .reservations;
        Ok(assess_epic_retirement(EpicRetirementSnapshot {
            tasks: &tasks,
            runs: &runs,
            reservations: &reservations,
        }))
    }
}

fn is_epic_tagged(task: &Task) -> bool {
    has_epic_tag(&task.tags)
}

/// Every `epic`-tagged root plus every descendant of one, by ID.
fn epic_family<'a>(by_id: &BTreeMap<&'a str, &'a Task>) -> BTreeSet<&'a str> {
    by_id
        .iter()
        .filter(|(_, task)| is_epic_tagged(task) || epic_root_of(task, by_id).is_some())
        .map(|(task_id, _)| *task_id)
        .collect()
}

/// The nearest `epic`-tagged ancestor of `task`, if any. Cycle- and
/// depth-guarded, like the admission-side walk it replaces.
fn epic_root_of<'a>(task: &Task, by_id: &BTreeMap<&'a str, &'a Task>) -> Option<&'a str> {
    let mut visited = BTreeSet::from([task.id.as_str().to_string()]);
    let mut next_parent_id = task.parent_id();
    for _ in 0..MAX_TASK_PARENT_CHAIN_DEPTH {
        let parent_id = next_parent_id?;
        if !visited.insert(parent_id.to_string()) {
            return None;
        }
        let (parent_id, parent) = by_id.get_key_value(parent_id)?;
        if is_epic_tagged(parent) {
            return Some(parent_id);
        }
        next_parent_id = parent.parent_id();
    }
    None
}

fn inherited_only_roots(by_id: &BTreeMap<&str, &Task>) -> Vec<InheritedOnlyRoot> {
    inherited_only_epic_roots(by_id.values().map(|task| EpicHierarchyNode::from(*task)))
        .into_iter()
        .map(|(task_id, descendants_with_context)| InheritedOnlyRoot {
            task_id: task_id.to_string(),
            status: by_id
                .get(task_id)
                .map(|task| task.status.to_string())
                .unwrap_or_default(),
            descendants_with_context: descendants_with_context
                .into_iter()
                .map(ToOwned::to_owned)
                .collect(),
        })
        .collect()
}

/// Task IDs a stored run input names, across every shape the retired machinery
/// used: `task_ids`, a singular `task_id`, and an epic root's `epic_task_id`.
fn run_task_ids(run: &JobRun) -> Vec<String> {
    let Some(input) = run.input.as_ref() else {
        return Vec::new();
    };
    let mut ids: Vec<String> = input
        .get("task_ids")
        .and_then(Value::as_array)
        .map(|ids| {
            ids.iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default();
    for key in ["task_id", "epic_task_id"] {
        if let Some(id) = input
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty())
        {
            ids.push(id.to_string());
        }
    }
    ids.sort();
    ids.dedup();
    ids
}
