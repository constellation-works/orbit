//! Shared bounded task queries for the runtime task-list surface.

use std::collections::{BTreeMap, BTreeSet};

use orbit_common::{NotFoundKind, OrbitError};
use orbit_store::TaskStoreBackend;
use orbit_types::task::{Task, TaskStatus, task_dependencies_ready};

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

/// Readiness and path matching retain their existing application policy. These
/// residual predicates hydrate metadata matches before applying the limit.
fn query_task_store(
    store: &dyn TaskStoreBackend,
    query: &TaskListQuery,
) -> Result<TaskPage, OrbitError> {
    let residual = |task: &Task, statuses: &BTreeMap<String, TaskStatus>| {
        (!query.ready || task_dependencies_ready(task, statuses))
            && query
                .path
                .as_deref()
                .is_none_or(|path| crate::task_selectors_contain_path(&task.context_files, path))
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
