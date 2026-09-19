use std::collections::BTreeMap;

use orbit_common::{NotFoundKind, OrbitError};
use orbit_types::task::{
    ArtifactManifestFileV2, ExternalRef, Task, TaskArtifact, TaskComment, TaskHistoryEntry,
};

use orbit_store::RegisteredTaskResolution;

use crate::OrbitRuntime;

impl OrbitRuntime {
    pub fn get_task(&self, id: &str) -> Result<Task, OrbitError> {
        if self.worker_invocation().is_some() {
            return self.read_owner(id, "task");
        }
        self.stores()
            .tasks()
            .get_task(id)?
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::Task, id.to_string()))
    }

    /// Resolve a dependency target through this machine's registered owner.
    ///
    /// A dependency is the one task reference that legitimately crosses a
    /// workspace boundary: task ids are globally unique, and readiness already
    /// projects statuses across every registered workspace
    /// ([`Self::task_status_index`]). Reading the dependency's body has to
    /// follow the same ownership, or preparation reports a prerequisite that
    /// admission can plainly see as `not found` [ORB-12544].
    ///
    /// Selected-task authority is unchanged: this runtime still acts in, and
    /// writes to, its own workspace only. The resolution never falls back to
    /// another host, never accepts a caller-supplied owner, and never mutates
    /// the prerequisite's workspace.
    pub fn resolve_dependency_task(
        &self,
        id: &str,
    ) -> Result<RegisteredTaskResolution, OrbitError> {
        if self.worker_invocation().is_some() {
            return self
                .read_owner(id, "dependency")
                .map(|task| RegisteredTaskResolution::Resolved(Box::new(task)));
        }
        self.stores().tasks().registered_task(id)
    }

    pub fn get_task_artifacts(&self, id: &str) -> Result<Vec<TaskArtifact>, OrbitError> {
        if self.worker_invocation().is_some() {
            return self.read_owner(id, "artifacts");
        }
        self.stores()
            .task_artifacts()
            .get_task_artifacts(id)?
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::Task, id.to_string()))
    }

    pub fn get_task_artifact_manifest(
        &self,
        id: &str,
    ) -> Result<Vec<ArtifactManifestFileV2>, OrbitError> {
        if self.worker_invocation().is_some() {
            return self.read_owner(id, "manifest");
        }
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
        if self.worker_invocation().is_some() {
            return Ok(self
                .get_task_artifacts(id)?
                .into_iter()
                .find(|artifact| artifact.path == path));
        }

        self.stores().task_artifacts().get_task_artifact(id, path)
    }

    pub fn get_task_comments(&self, id: &str) -> Result<Vec<TaskComment>, OrbitError> {
        if self.worker_invocation().is_some() {
            return self.read_owner(id, "comments");
        }
        self.stores()
            .task_history()
            .get_task_comments(id)?
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::Task, id.to_string()))
    }

    pub fn get_task_history(&self, id: &str) -> Result<Vec<TaskHistoryEntry>, OrbitError> {
        if self.worker_invocation().is_some() {
            return self.read_owner(id, "history");
        }
        self.stores()
            .task_history()
            .get_task_history(id)?
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::Task, id.to_string()))
    }

    pub fn list_tasks(&self) -> Result<Vec<Task>, OrbitError> {
        if self.worker_invocation().is_some() {
            return self.read_owner("", "tasks");
        }

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
        if self.worker_invocation().is_some() {
            return self.read_owner("", "status_index");
        }

        if !self.coordination_task_reads_visible() {
            return Ok(BTreeMap::new());
        }
        self.stores().tasks().task_status_index()
    }

    /// Status distribution per complexity bucket from the generated task index.
    pub fn task_completion_by_complexity(
        &self,
    ) -> Result<Vec<orbit_store::TaskCompletionByComplexity>, OrbitError> {
        if self.worker_invocation().is_some() {
            let rows: Vec<(String, i64, BTreeMap<String, i64>)> =
                self.read_owner("", "completion_by_complexity")?;
            return Ok(rows
                .into_iter()
                .map(
                    |(complexity, total, by_status)| orbit_store::TaskCompletionByComplexity {
                        complexity,
                        total,
                        by_status,
                    },
                )
                .collect());
        }
        if !self.coordination_task_reads_visible() {
            return Ok(Vec::new());
        }
        self.stores().tasks().task_completion_by_complexity()
    }

    /// `task_id →` complexity bucket from the generated task index.
    pub fn task_complexity_by_id(&self) -> Result<BTreeMap<String, String>, OrbitError> {
        if self.worker_invocation().is_some() {
            return self.read_owner("", "complexity_by_id");
        }

        if !self.coordination_task_reads_visible() {
            return Ok(BTreeMap::new());
        }
        self.stores().tasks().task_complexity_by_id()
    }

    pub fn list_tasks_by_tags(&self, tags: &[String]) -> Result<Vec<Task>, OrbitError> {
        if self.worker_invocation().is_some() {
            return self
                .read_owner_request(serde_json::json!({"_worker_read": "tags", "tags": tags}));
        }
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
        if self.worker_invocation().is_some() {
            return self.read_owner_request(serde_json::json!({"_worker_read": "filtered", "status": status, "priority": priority, "parent_id": parent_id, "job_run_id": job_run_id, "external_ref": external_ref, "has_external_ref_system": has_external_ref_system}));
        }
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
        if self.worker_invocation().is_some() {
            return self.search_tasks_filtered(query, &[]);
        }
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
        if self.worker_invocation().is_some() {
            return self.read_owner_request(
                serde_json::json!({"_worker_read": "search", "query": query, "tags": tags}),
            );
        }
        if !self.coordination_task_reads_visible() {
            return Ok(Vec::new());
        }
        self.stores().tasks().search_tasks_filtered(query, tags)
    }
}
