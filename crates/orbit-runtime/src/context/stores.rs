use crate::plugin::host::PluginHostLoad;
use crate::stored::job_runs::JobRuns;
use orbit_config::{CodexExecutionPolicy, ExecutionEnvPolicy};
use orbit_policy::PolicyEngine;
use orbit_search::LexicalIndex;
use orbit_store::compose::WorkspaceTaskBackends;
use orbit_store::skill_store::SkillDirs;
use orbit_store::{ExecutorDefDir, FileStore, PolicyDefDirs, Store};
use orbit_store::{
    TaskArtifactRepository, TaskDocumentRepository, TaskHistoryRepository,
    TaskReservationRepository,
};
use orbit_store::{TaskCoordination, TaskV2Store};
use orbit_tools::ToolRegistry;
use std::sync::Arc;

/// Host SQLite handle.
///
/// Accessors clone the handle; they must not call `Store::open` again.
#[derive(Clone)]
pub(crate) struct OrbitHostStore {
    pub(crate) sqlite: Store,
}

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
    pub(crate) host: OrbitHostStore,
}

impl OrbitStores {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        tasks: WorkspaceTaskBackends,
        lexical_index: LexicalIndex,
        task_reservation: TaskReservationRepository,
        job_run: Arc<JobRuns>,
        executor_def: FileStore<ExecutorDefDir>,
        policy_def: FileStore<PolicyDefDirs>,
        host: OrbitHostStore,
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
        &self.host.sqlite
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

#[derive(Clone)]
pub(crate) struct OrbitExecutionAssets {
    pub(crate) registry: Arc<ToolRegistry>,
    pub(crate) skill_catalog: FileStore<SkillDirs>,
    /// What the host plugin load pass registered and refused, kept so
    /// `orbit plugin list`/`show`/`doctor` report the same facts the tool
    /// surface was built from rather than re-deriving them.
    pub(crate) plugins: Arc<PluginHostLoad>,
}

impl OrbitExecutionAssets {
    pub(crate) fn new(
        registry: Arc<ToolRegistry>,
        skill_catalog: FileStore<SkillDirs>,
        plugins: PluginHostLoad,
    ) -> Self {
        Self {
            registry,
            skill_catalog,
            plugins: Arc::new(plugins),
        }
    }
}

#[derive(Clone)]
pub(crate) struct OrbitPolicyContext {
    pub(crate) policy: PolicyEngine,
    pub(crate) execution_env_policy: ExecutionEnvPolicy,
    pub(crate) codex_execution_policy: CodexExecutionPolicy,
}

impl OrbitPolicyContext {
    pub(crate) fn new(
        policy: PolicyEngine,
        execution_env_policy: ExecutionEnvPolicy,
        codex_execution_policy: CodexExecutionPolicy,
    ) -> Self {
        Self {
            policy,
            execution_env_policy,
            codex_execution_policy,
        }
    }
}
