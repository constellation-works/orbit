use std::sync::Arc;

use orbit_common::OrbitError;

use crate::boundary::TaskV2Store;
use crate::contracts::TaskDocumentUpdateParams;

/// Document records of a workspace's task store.
#[derive(Clone)]
pub struct TaskDocumentRepository {
    pub(crate) store: Arc<TaskV2Store>,
}

impl TaskDocumentRepository {
    pub fn update_task_document(
        &self,
        id: &str,
        params: TaskDocumentUpdateParams,
    ) -> Result<crate::contracts::TaskRow, OrbitError> {
        self.store.update_task_document(id, &params)
    }

    /// Rewrite the task's stored pre-family tool names (`required_tools`,
    /// `granted_tools`) to family actions and record the rewrite in its
    /// history. `Ok(false)` when it holds none; no other field changes.
    pub fn migrate_task_tool_names(&self, id: &str, actor: &str) -> Result<bool, OrbitError> {
        self.store.migrate_task_tool_names(id, actor)
    }
}
