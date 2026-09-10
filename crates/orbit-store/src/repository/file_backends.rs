use std::collections::BTreeMap;

use orbit_common::OrbitError;
use orbit_types::policy::PolicyDef;
use orbit_types::task::{
    ArtifactManifestFileV2, ExternalRef, Task, TaskArtifact, TaskComment, TaskHistoryEntry,
    TaskPriority, TaskStatus,
};
use orbit_types::workflow::ExecutorDef;

use crate::contracts::{
    AtomicTaskMutationOutcome, AtomicTaskMutationParams, ExecutorDefStoreBackend,
    PolicyDefStoreBackend, TaskArtifactStoreBackend, TaskArtifactUpdateParams, TaskCreateParams,
    TaskDocumentStoreBackend, TaskDocumentUpdateParams, TaskHistoryStoreBackend,
    TaskHistoryUpdateParams, TaskStoreBackend,
};
use crate::driver::file::executor_def_store::ExecutorDefFileStore;
use crate::driver::file::policy_def_store::PolicyDefFileStore;
use crate::repository::task::TaskV2Store;
use crate::scope::{ScopeStrategy, ScopedStore, resolve};

impl TaskStoreBackend for TaskV2Store {
    fn create_task_idempotent(
        &self,
        params: TaskCreateParams,
        key: &str,
    ) -> Result<Task, OrbitError> {
        self.create_task_with_key(params, Some(key))
    }

    fn task_candidates(
        &self,
        filter: &crate::contracts::TaskListFilter,
        limit: usize,
    ) -> Result<crate::contracts::TaskCandidates, OrbitError> {
        self.task_candidates(filter, limit)
    }
    fn query_task_rows(
        &self,
        filter: &crate::contracts::TaskListFilter,
        limit: usize,
        residual: crate::contracts::TaskResidualFilter<'_>,
    ) -> Result<crate::contracts::TaskPage, OrbitError> {
        self.query_task_rows(filter, limit, residual)
    }
    fn get_task_row(
        &self,
        id: &str,
        list_read: bool,
    ) -> Result<Option<crate::contracts::TaskRow>, OrbitError> {
        self.get_task_row(id, list_read)
    }

    fn create_task(&self, params: TaskCreateParams) -> Result<Task, OrbitError> {
        self.create_task(params)
    }

    fn list_tasks(&self) -> Result<Vec<Task>, OrbitError> {
        self.list_tasks()
    }

    fn task_status_index(&self) -> Result<BTreeMap<String, TaskStatus>, OrbitError> {
        self.task_status_index()
    }

    fn list_tasks_by_tags(&self, tags: &[String]) -> Result<Vec<Task>, OrbitError> {
        self.list_tasks_by_tags(tags)
    }

    fn list_tasks_filtered(
        &self,
        status: Option<TaskStatus>,
        priority: Option<TaskPriority>,
        parent_id: Option<&str>,
        job_run_id: Option<&str>,
        external_ref: Option<&ExternalRef>,
        has_external_ref_system: Option<&str>,
    ) -> Result<Vec<Task>, OrbitError> {
        self.list_tasks_filtered(
            status,
            priority,
            parent_id,
            job_run_id,
            external_ref,
            has_external_ref_system,
        )
    }

    fn get_task(&self, id: &str) -> Result<Option<Task>, OrbitError> {
        resolve::<Task, _>(self, id)
    }

    fn search_tasks(&self, query: &str) -> Result<Vec<Task>, OrbitError> {
        self.search_tasks(query)
    }

    fn search_tasks_filtered(&self, query: &str, tags: &[String]) -> Result<Vec<Task>, OrbitError> {
        self.search_tasks_filtered(query, tags)
    }

    fn delete_task(&self, id: &str) -> Result<bool, OrbitError> {
        self.delete_task(id)
    }

    fn with_task_write_lock(
        &self,
        id: &str,
        op: &mut dyn FnMut() -> Result<(), OrbitError>,
    ) -> Result<(), OrbitError> {
        self.with_task_lock(id, op)
    }

    fn apply_atomic_task_mutation(
        &self,
        id: &str,
        params: &AtomicTaskMutationParams,
    ) -> Result<AtomicTaskMutationOutcome, OrbitError> {
        self.apply_atomic_task_mutation(id, params)
    }

    fn task_completion_by_complexity(
        &self,
    ) -> Result<Vec<crate::contracts::TaskCompletionByComplexity>, OrbitError> {
        self.task_completion_by_complexity()
    }

    fn task_complexity_by_id(&self) -> Result<BTreeMap<String, String>, OrbitError> {
        self.task_complexity_by_id()
    }
}

impl ScopedStore<Task> for TaskV2Store {
    type Err = OrbitError;

    fn strategy(&self) -> ScopeStrategy {
        ScopeStrategy::WorkspaceOnly
    }

    fn get_workspace(&self, key: &str) -> Result<Option<Task>, OrbitError> {
        self.get_task(key)
    }

    fn get_global(&self, _key: &str) -> Result<Option<Task>, OrbitError> {
        Ok(None)
    }
}

impl TaskDocumentStoreBackend for TaskV2Store {
    fn update_task_document(
        &self,
        id: &str,
        params: TaskDocumentUpdateParams,
    ) -> Result<(), OrbitError> {
        self.update_task_document(id, &params)
    }
}

impl TaskHistoryStoreBackend for TaskV2Store {
    fn get_task_comments(&self, id: &str) -> Result<Option<Vec<TaskComment>>, OrbitError> {
        self.get_task_comments(id)
    }

    fn get_task_history(&self, id: &str) -> Result<Option<Vec<TaskHistoryEntry>>, OrbitError> {
        self.get_task_history(id)
    }

    fn update_task_history(
        &self,
        id: &str,
        params: TaskHistoryUpdateParams,
    ) -> Result<(), OrbitError> {
        self.update_task_history(id, &params)
    }
}

impl TaskArtifactStoreBackend for TaskV2Store {
    fn get_task_artifact_manifest(
        &self,
        id: &str,
    ) -> Result<Option<Vec<ArtifactManifestFileV2>>, OrbitError> {
        self.get_task_artifact_manifest(id)
    }

    fn get_task_artifacts(&self, id: &str) -> Result<Option<Vec<TaskArtifact>>, OrbitError> {
        self.get_task_artifacts(id)
    }

    fn get_task_artifact(&self, id: &str, path: &str) -> Result<Option<TaskArtifact>, OrbitError> {
        self.get_task_artifact(id, path)
    }

    fn upsert_task_artifacts(
        &self,
        id: &str,
        params: TaskArtifactUpdateParams,
    ) -> Result<(), OrbitError> {
        self.upsert_task_artifacts(id, &params)
    }
}

impl ExecutorDefStoreBackend for ExecutorDefFileStore {
    fn list_executor_defs(&self) -> Result<Vec<ExecutorDef>, OrbitError> {
        self.list_executor_defs()
    }

    fn get_executor_def(&self, name: &str) -> Result<Option<ExecutorDef>, OrbitError> {
        self.get_executor_def(name)
    }

    fn upsert_executor_def(&self, def: &ExecutorDef) -> Result<(), OrbitError> {
        self.upsert_executor_def(def)
    }
}

impl PolicyDefStoreBackend for PolicyDefFileStore {
    fn list_policy_defs(&self) -> Result<Vec<PolicyDef>, OrbitError> {
        self.list_policy_defs()
    }

    fn get_policy_def(&self, name: &str) -> Result<Option<PolicyDef>, OrbitError> {
        self.get_policy_def(name)
    }

    fn upsert_policy_def(&self, def: &PolicyDef) -> Result<(), OrbitError> {
        self.upsert_policy_def(def)
    }
}
