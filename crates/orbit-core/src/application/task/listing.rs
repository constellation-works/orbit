//! Shared bounded task queries for the runtime task-list surface.

use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet};

use orbit_common::{NotFoundKind, OrbitError};
use orbit_store::TaskStoreBackend;
use orbit_types::task::{
    Task, TaskReferenceIndex, TaskStatus, automatic_dispatch_cmp,
    task_dependencies_ready_with_index,
};

use crate::OrbitRuntime;
use crate::runtime::task::locks::{
    TaskLockOverlap, active_task_lock_holders, lock_holder_index, task_lock_overlaps,
};

pub use orbit_store::contracts::{
    TaskCandidateKey, TaskCandidateKeys, TaskCandidates, TaskListFilter, TaskPage, TaskRow,
};

#[derive(Debug)]
pub struct TaskListQuery {
    pub filter: TaskListFilter,
    pub ready: bool,
    pub path: Option<String>,
    pub limit: usize,
}

impl Default for TaskListQuery {
    fn default() -> Self {
        Self {
            filter: TaskListFilter::default(),
            ready: false,
            path: None,
            limit: crate::DEFAULT_TASK_LIST_LIMIT,
        }
    }
}

/// Statuses `orbit task eligible` selects candidates from: work not yet taken.
/// `in-progress` and `review` tasks are the holders a candidate is checked
/// against, never candidates themselves.
const ELIGIBILITY_CANDIDATE_STATUSES: [TaskStatus; 2] = [TaskStatus::Backlog, TaskStatus::Proposed];

/// Which not-yet-taken tasks can be picked up without colliding with work in
/// flight.
pub(crate) struct TaskEligibilityQuery {
    /// Candidate statuses; empty selects every
    /// [`ELIGIBILITY_CANDIDATE_STATUSES`] entry.
    pub(crate) statuses: Vec<TaskStatus>,
    /// The `task list --path` selector match, applied to candidates.
    pub(crate) path: Option<String>,
    /// Maximum eligible tasks returned.
    pub(crate) limit: usize,
}

/// A candidate held back by in-flight work, with every overlap that holds it.
pub(crate) struct TaskEligibilityConflict {
    pub(crate) task: Task,
    pub(crate) overlaps: Vec<TaskLockOverlap>,
}

/// Candidates split by whether their lock surface overlaps any `in-progress`
/// or `review` task's surface. Both lists are in automatic dispatch order and
/// hold envelope metadata (no body documents).
#[derive(Default)]
pub(crate) struct TaskEligibility {
    /// Eligible candidates, at most the query's limit.
    pub(crate) eligible: Vec<Task>,
    /// Eligible candidates before the limit.
    pub(crate) total: usize,
    /// Every conflicting candidate; not limited.
    pub(crate) conflicting: Vec<TaskEligibilityConflict>,
}

/// Every task `store` lists, newest first, without body documents:
/// `description`, `acceptance_criteria`, `plan` and `execution_summary` are
/// empty and every envelope field is populated as a full read populates it.
///
/// One index-validated envelope read per task instead of a bundle read, for
/// callers that read status, attribution, tags, relations, context files or
/// timestamps of the whole workspace.
pub(crate) fn list_task_metadata_in(store: &dyn TaskStoreBackend) -> Result<Vec<Task>, OrbitError> {
    Ok(store
        .task_candidates(&TaskListFilter::default(), usize::MAX)?
        .items
        .into_iter()
        .map(|envelope| {
            Task::from_envelope_parts(
                envelope,
                String::new(),
                Vec::new(),
                String::new(),
                String::new(),
            )
        })
        .collect())
}

/// Readiness and path matching retain their existing application policy. Both
/// are decided from envelope metadata, so the store applies them to every
/// candidate before the limit and hydrates only the rows that fill the page.
fn query_task_store(
    store: &dyn TaskStoreBackend,
    query: &TaskListQuery,
) -> Result<TaskPage, OrbitError> {
    // The store hands every call of one query the same status projection, so
    // the prefix knowledge readiness needs is derived from it once rather than
    // by a scan of the whole projection for each candidate.
    let reference_index = OnceCell::new();
    let residual = |task: &Task, statuses: &BTreeMap<String, TaskStatus>| {
        (!query.ready
            || task_dependencies_ready_with_index(
                task,
                statuses,
                reference_index.get_or_init(|| TaskReferenceIndex::from_status_index(statuses)),
            ))
            && query.path.as_deref().is_none_or(|path| {
                crate::application::search::task_selectors_contain_path(&task.context_files, path)
            })
    };
    store.query_task_rows(
        &query.filter,
        query.limit,
        (query.ready || query.path.is_some()).then_some(&residual),
    )
}

impl OrbitRuntime {
    pub fn query_task_rows(&self, query: &TaskListQuery) -> Result<TaskPage, OrbitError> {
        if !self.coordination_task_reads_visible() {
            return Ok(TaskPage::default());
        }
        query_task_store(self.stores().tasks(), query)
    }

    /// Query every lifecycle status with active work first, preserving newest
    /// first ordering within each status bucket. One candidate scan serves
    /// both buckets: the store partitions the ordered candidates so the page
    /// fills with non-terminal tasks before any terminal one.
    pub fn query_task_rows_status_aware(
        &self,
        query: &TaskListQuery,
    ) -> Result<TaskPage, OrbitError> {
        if query.filter.statuses.is_some() {
            return self.query_task_rows(query);
        }
        self.query_task_rows(&TaskListQuery {
            filter: TaskListFilter {
                terminal_last: true,
                ..query.filter.clone()
            },
            ready: query.ready,
            path: query.path.clone(),
            limit: query.limit,
        })
    }

    pub fn task_candidates(
        &self,
        filter: &TaskListFilter,
        limit: usize,
    ) -> Result<TaskCandidates, OrbitError> {
        if !self.coordination_task_reads_visible() {
            return Ok(TaskCandidates::default());
        }
        self.stores().tasks().task_candidates(filter, limit)
    }

    /// [`Self::task_candidates`] answered by the generated index alone, for a
    /// filter it fully covers: the selected ids and creation times, with no
    /// envelope read. `None` sends the caller to [`Self::task_candidates`].
    pub fn task_candidate_keys(
        &self,
        filter: &TaskListFilter,
        limit: usize,
    ) -> Result<Option<TaskCandidateKeys>, OrbitError> {
        if !self.coordination_task_reads_visible() {
            return Ok(Some(Default::default()));
        }
        self.stores().tasks().task_candidate_keys(filter, limit)
    }

    /// Return the bounded registry status projection needed to label one
    /// task's dependency and relation targets. The registry resolves target
    /// ids across workspace partitions without hydrating their bundles.
    pub fn task_status_index_for(
        &self,
        targets: &BTreeSet<String>,
    ) -> Result<BTreeMap<String, TaskStatus>, OrbitError> {
        if !self.coordination_task_reads_visible() {
            return Ok(BTreeMap::new());
        }
        let workspace_id = self.workspace_id()?;
        self.stores()
            .tasks()
            .task_status_index_for(&workspace_id, targets)
    }

    pub fn get_task_row(&self, id: &str) -> Result<TaskRow, OrbitError> {
        self.stores()
            .tasks()
            .get_task_row(id, false)?
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::Task, id.to_string()))
    }

    pub fn get_listed_task_row(&self, id: &str) -> Result<Option<TaskRow>, OrbitError> {
        if !self.coordination_task_reads_visible() {
            return Ok(None);
        }
        self.stores().tasks().get_task_row(id, true)
    }

    /// Split candidate tasks by lock conflict with in-flight work.
    ///
    /// Lock overlap is the only test, and it is the one automatic admission
    /// applies: the same surface expansion, the same `in-progress` / `review`
    /// holder map and the same overlap index. None of admission's other gates
    /// apply — dependencies, complexity or preparation, group and epic
    /// roll-ups, crew — and candidates are not checked against one another.
    /// Nothing is reserved or written.
    pub(crate) fn task_eligibility(
        &self,
        query: &TaskEligibilityQuery,
    ) -> Result<TaskEligibility, OrbitError> {
        if let Some(status) = query
            .statuses
            .iter()
            .find(|status| !ELIGIBILITY_CANDIDATE_STATUSES.contains(status))
        {
            return Err(OrbitError::InvalidInput(format!(
                "eligibility candidates are `backlog` or `proposed` tasks; `{status}` is not a candidate status"
            )));
        }
        if !self.coordination_task_reads_visible() {
            return Ok(TaskEligibility::default());
        }
        let statuses: &[TaskStatus] = if query.statuses.is_empty() {
            &ELIGIBILITY_CANDIDATE_STATUSES
        } else {
            &query.statuses
        };
        let tasks = list_task_metadata_in(self.stores().tasks())?;
        let workspace_root = self.paths().repo_root.as_path();
        let lock_holders = active_task_lock_holders(&tasks, workspace_root);
        let holder_index = lock_holder_index(&lock_holders);
        let mut candidates: Vec<&Task> = tasks
            .iter()
            .filter(|task| statuses.contains(&task.status))
            .filter(|task| {
                query.path.as_deref().is_none_or(|path| {
                    crate::application::search::task_selectors_contain_path(
                        &task.context_files,
                        path,
                    )
                })
            })
            .collect();
        candidates.sort_by(|left, right| automatic_dispatch_cmp(left, right));

        let mut eligibility = TaskEligibility::default();
        for task in candidates {
            let overlaps = task_lock_overlaps(task, &holder_index, workspace_root);
            if !overlaps.is_empty() {
                eligibility.conflicting.push(TaskEligibilityConflict {
                    task: task.clone(),
                    overlaps,
                });
                continue;
            }
            eligibility.total += 1;
            if eligibility.eligible.len() < query.limit {
                eligibility.eligible.push(task.clone());
            }
        }
        Ok(eligibility)
    }
}
