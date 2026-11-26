use std::sync::Arc;

use orbit_common::OrbitError;
use orbit_types::task::{TaskComment, TaskHistoryEntry};

use crate::boundary::TaskV2Store;
use crate::contract::TaskHistoryUpdateParams;

/// History records of a workspace's task store.
#[derive(Clone)]
pub struct TaskHistoryRepository {
    pub(crate) store: Arc<TaskV2Store>,
}

impl TaskHistoryRepository {
    pub fn get_task_comments(&self, id: &str) -> Result<Option<Vec<TaskComment>>, OrbitError> {
        self.store.read_task_comments(id)
    }

    pub fn get_task_history(&self, id: &str) -> Result<Option<Vec<TaskHistoryEntry>>, OrbitError> {
        self.store.read_task_history(id)
    }

    /// Read the event history of each listed task in request order, checking
    /// partition settledness once per batch rather than per task. Each entry
    /// preserves the outcome [`Self::get_task_history`] would return for that
    /// task (`Ok(Some(entries))`, `Ok(None)` when missing, `Err` when corrupt
    /// or unreadable).
    pub fn get_task_histories(
        &self,
        ids: &[String],
    ) -> Vec<crate::contract::TaskHistoryReadOutcome> {
        self.store.read_task_histories(ids)
    }

    /// Evidence that one task's history and comments are unchanged. Kept as
    /// a compatibility wrapper over the batch operation; aggregate readers
    /// should use [`Self::task_history_stamps`] to avoid a check per task.
    pub fn task_history_stamp(&self, id: &str) -> Result<Option<String>, OrbitError> {
        let ids = [id.to_string()];
        Ok(self.task_history_stamps(&ids)?.remove(id))
    }

    /// Evidence that a task's history and comments are unchanged, for each
    /// listed task, in one pass.
    pub fn task_history_stamps(
        &self,
        ids: &[String],
    ) -> Result<std::collections::BTreeMap<String, String>, OrbitError> {
        self.store.task_history_stamps(ids)
    }

    /// Whole-bundle unchanged evidence for each listed task.
    pub fn task_bundle_stamps(
        &self,
        ids: &[String],
    ) -> Result<std::collections::BTreeMap<String, String>, OrbitError> {
        self.store.task_bundle_stamps(ids)
    }

    pub fn update_task_history(
        &self,
        id: &str,
        params: TaskHistoryUpdateParams,
    ) -> Result<crate::contract::TaskRow, OrbitError> {
        self.store.update_task_history(id, &params)
    }

    /// Refuse a work-starting history write overlapping a protecting claim.
    pub fn check_ordinary_work_start_footprint(
        &self,
        id: &str,
        status: Option<orbit_types::task::TaskStatus>,
        status_event: Option<&str>,
        context_files: Option<&[String]>,
    ) -> Result<(), OrbitError> {
        self.store
            .check_ordinary_work_start_footprint(id, status, status_event, context_files)
    }
}
