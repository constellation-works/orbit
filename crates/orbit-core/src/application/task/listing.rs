//! Shared bounded task queries for the runtime task-list surface.

use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet};

use orbit_common::{NotFoundKind, OrbitError};
use orbit_store::TaskStoreBackend;
use orbit_types::task::{Task, TaskReferenceIndex, TaskStatus, task_dependencies_ready_with_index};

use crate::OrbitRuntime;

pub use orbit_store::contracts::{TaskCandidates, TaskListFilter, TaskPage, TaskRow};

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
}
