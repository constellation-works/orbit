use super::job_runs::JobRuns;
use orbit_search::LexicalIndex;
use orbit_store::compose::WorkspaceTaskBackends;
use orbit_store::{ExecutorDefDir, FileStore, PolicyDefDirs, Store};
use orbit_store::{
    TaskArtifactRepository, TaskDocumentRepository, TaskHistoryRepository,
    TaskReservationRepository,
};
use orbit_store::{TaskCoordination, TaskV2Store};
use std::sync::Arc;

#[derive(Clone)]
pub struct OrbitStores {
    pub(crate) task: Arc<TaskV2Store>,
    task_coordination: TaskCoordination,
    pub(crate) task_document: TaskDocumentRepository,
    pub(crate) task_history: TaskHistoryRepository,
    pub(crate) task_artifact: TaskArtifactRepository,
    pub(crate) lexical_index: LexicalIndex,
    pub(crate) task_reservation: TaskReservationRepository,
    pub job_run: Arc<JobRuns>,
    pub(crate) executor_def: FileStore<ExecutorDefDir>,
    pub(crate) policy_def: FileStore<PolicyDefDirs>,
    /// Host SQLite handle. Accessors clone it; they must not call `Store::open` again.
    pub(crate) host: Store,
}

impl OrbitStores {
    pub(crate) fn new(
        tasks: WorkspaceTaskBackends,
        lexical_index: LexicalIndex,
        task_reservation: TaskReservationRepository,
        job_run: Arc<JobRuns>,
        executor_def: FileStore<ExecutorDefDir>,
        policy_def: FileStore<PolicyDefDirs>,
        host: Store,
    ) -> Self {
        Self {
            task: tasks.task,
            task_coordination: tasks.coordination,
            task_document: tasks.document,
            task_history: tasks.history,
            task_artifact: tasks.artifact,
            lexical_index,
            task_reservation,
            job_run,
            executor_def,
            policy_def,
            host,
        }
    }

    pub fn tasks(&self) -> &TaskV2Store {
        self.task.as_ref()
    }

    /// This workspace's task coordination through the shared commit boundary.
    pub fn task_coordination(&self) -> &TaskCoordination {
        &self.task_coordination
    }

    pub fn task_documents(&self) -> &TaskDocumentRepository {
        &self.task_document
    }

    pub fn task_history(&self) -> &TaskHistoryRepository {
        &self.task_history
    }

    pub fn task_artifacts(&self) -> &TaskArtifactRepository {
        &self.task_artifact
    }

    /// The optional lexical index. Callers reach its store through
    /// [`LexicalIndex::store`], which names the absence when there is none.
    pub fn lexical_index(&self) -> &LexicalIndex {
        &self.lexical_index
    }

    pub fn task_reservations(&self) -> &TaskReservationRepository {
        &self.task_reservation
    }

    /// This workspace's job runs, local pull admissions and drain intent.
    pub fn jobs(&self) -> &JobRuns {
        self.job_run.as_ref()
    }

    /// The host SQLite store: tool, plugin and script grant records, among
    /// others, reached as `read()?.<record>()` and `write(|tx| tx.<record>())`.
    pub fn host_store(&self) -> &Store {
        &self.host
    }

    /// The global executor definitions.
    pub fn executors(&self) -> &FileStore<ExecutorDefDir> {
        &self.executor_def
    }

    /// The workspace policy definitions layered over the global ones.
    pub(crate) fn policies(&self) -> &FileStore<PolicyDefDirs> {
        &self.policy_def
    }
}
