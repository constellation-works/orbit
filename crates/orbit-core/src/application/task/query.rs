use std::collections::BTreeMap;

use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::task::{
    ArtifactManifestFileV2, ExternalRef, Task, TaskArtifact, TaskComment, TaskHistoryEntry,
};

use crate::OrbitRuntime;

impl OrbitRuntime {
    pub fn get_task(&self, id: &str) -> Result<Task, OrbitError> {
        self.stores()
            .tasks()
            .get_task(id)?
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::Task, id.to_string()))
    }

    pub fn get_task_artifacts(&self, id: &str) -> Result<Vec<TaskArtifact>, OrbitError> {
        self.stores()
            .task_artifacts()
            .get_task_artifacts(id)?
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::Task, id.to_string()))
    }

    pub fn get_task_artifact_manifest(
        &self,
        id: &str,
    ) -> Result<Vec<ArtifactManifestFileV2>, OrbitError> {
        self.stores()
            .task_artifacts()
            .get_task_artifact_manifest(id)?
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::Task, id.to_string()))
    }

    pub fn get_task_artifact(
        &self,
        id: &str,
        path: &str,
    ) -> Result<Option<TaskArtifact>, OrbitError> {
        self.stores().task_artifacts().get_task_artifact(id, path)
    }

    pub fn get_task_comments(&self, id: &str) -> Result<Vec<TaskComment>, OrbitError> {
        self.stores()
            .task_history()
            .get_task_comments(id)?
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::Task, id.to_string()))
    }

    pub fn get_task_history(&self, id: &str) -> Result<Vec<TaskHistoryEntry>, OrbitError> {
        self.stores()
            .task_history()
            .get_task_history(id)?
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::Task, id.to_string()))
    }

    pub fn list_tasks(&self) -> Result<Vec<Task>, OrbitError> {
        if !self.coordination_task_reads_visible() {
            return Ok(Vec::new());
        }
        self.stores().tasks().list_tasks()
    }

    /// Returns the coordination registry's global status projection for
    /// dependency readiness while leaving task listing workspace-scoped.
    pub fn task_status_index(
        &self,
    ) -> Result<BTreeMap<String, orbit_types::task::TaskStatus>, OrbitError> {
        if !self.coordination_task_reads_visible() {
            return Ok(BTreeMap::new());
        }
        self.stores().tasks().task_status_index()
    }

    /// Status distribution per complexity bucket from the generated task index.
    pub fn task_completion_by_complexity(
        &self,
    ) -> Result<Vec<orbit_store::TaskCompletionByComplexity>, OrbitError> {
        if !self.coordination_task_reads_visible() {
            return Ok(Vec::new());
        }
        self.stores().tasks().task_completion_by_complexity()
    }

    /// `task_id →` complexity bucket from the generated task index.
    pub fn task_complexity_by_id(&self) -> Result<BTreeMap<String, String>, OrbitError> {
        if !self.coordination_task_reads_visible() {
            return Ok(BTreeMap::new());
        }
        self.stores().tasks().task_complexity_by_id()
    }

    pub fn list_tasks_by_tags(&self, tags: &[String]) -> Result<Vec<Task>, OrbitError> {
        if !self.coordination_task_reads_visible() {
            return Ok(Vec::new());
        }
        self.stores().tasks().list_tasks_by_tags(tags)
    }

    pub fn list_tasks_filtered(
        &self,
        status: Option<orbit_types::task::TaskStatus>,
        priority: Option<orbit_types::task::TaskPriority>,
        parent_id: Option<&str>,
        job_run_id: Option<&str>,
        external_ref: Option<&ExternalRef>,
        has_external_ref_system: Option<&str>,
    ) -> Result<Vec<Task>, OrbitError> {
        if !self.coordination_task_reads_visible() {
            return Ok(Vec::new());
        }
        self.stores().tasks().list_tasks_filtered(
            status,
            priority,
            parent_id,
            job_run_id,
            external_ref,
            has_external_ref_system,
        )
    }

    pub fn search_tasks(&self, query: &str) -> Result<Vec<Task>, OrbitError> {
        if !self.coordination_task_reads_visible() {
            return Ok(Vec::new());
        }
        self.stores().tasks().search_tasks(query)
    }

    pub fn search_tasks_filtered(
        &self,
        query: &str,
        tags: &[String],
    ) -> Result<Vec<Task>, OrbitError> {
        if !self.coordination_task_reads_visible() {
            return Ok(Vec::new());
        }
        self.stores().tasks().search_tasks_filtered(query, tags)
    }
}
