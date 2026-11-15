use crate::plugin::host::PluginHostLoad;
use orbit_config::{CodexExecutionPolicy, ExecutionEnvPolicy};
use orbit_policy::PolicyEngine;
use orbit_search::LexicalIndex;
use orbit_store::Store;
use orbit_store::contracts::{
    AuditEventStoreBackend, AutomationStoreBackend, ExecutorDefStoreBackend,
    InvocationStoreBackend, JobRunStoreBackend, PluginStoreBackend, PolicyDefStoreBackend,
    ProviderLimitStoreBackend, ReviewStoreBackend, TaskArtifactStoreBackend,
    TaskDocumentStoreBackend, TaskHistoryStoreBackend, TaskReservationStoreBackend,
    TaskStoreBackend, ToolStoreBackend, V2AuditStoreBackend,
};
use orbit_store::skill_store::SkillCatalog;
use orbit_tools::ToolRegistry;
use std::sync::Arc;

/// Host SQLite handle plus the feature backends composed from it.
///
/// These wrap the same writer connection the runtime opened at construction.
/// Accessors clone the handles; they must not call `Store::open` again.
#[derive(Clone)]
pub(crate) struct OrbitHostStore {
    pub(crate) sqlite: Store,
    pub(crate) automation: Arc<dyn AutomationStoreBackend>,
    pub(crate) review: Arc<dyn ReviewStoreBackend>,
    pub(crate) v2_audit: Arc<dyn V2AuditStoreBackend>,
    pub(crate) invocation: Arc<dyn InvocationStoreBackend>,
    pub(crate) provider_limit: Arc<dyn ProviderLimitStoreBackend>,
}

#[derive(Clone)]
pub struct OrbitStores {
    pub(crate) task: Arc<dyn TaskStoreBackend>,
    pub(crate) task_document: Arc<dyn TaskDocumentStoreBackend>,
    pub(crate) task_history: Arc<dyn TaskHistoryStoreBackend>,
    pub(crate) task_artifact: Arc<dyn TaskArtifactStoreBackend>,
    pub(crate) lexical_index: LexicalIndex,
    pub(crate) task_reservation: Arc<dyn TaskReservationStoreBackend>,
    pub job_run: Arc<dyn JobRunStoreBackend>,
    pub(crate) tool: Arc<dyn ToolStoreBackend>,
    pub(crate) plugin: Arc<dyn PluginStoreBackend>,
    pub audit_event: Arc<dyn AuditEventStoreBackend>,
    pub(crate) executor_def: Arc<dyn ExecutorDefStoreBackend>,
    pub(crate) policy_def: Arc<dyn PolicyDefStoreBackend>,
    pub(crate) host: OrbitHostStore,
}

impl OrbitStores {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        task: Arc<dyn TaskStoreBackend>,
        task_document: Arc<dyn TaskDocumentStoreBackend>,
        task_history: Arc<dyn TaskHistoryStoreBackend>,
        task_artifact: Arc<dyn TaskArtifactStoreBackend>,
        lexical_index: LexicalIndex,
        task_reservation: Arc<dyn TaskReservationStoreBackend>,
        job_run: Arc<dyn JobRunStoreBackend>,
        tool: Arc<dyn ToolStoreBackend>,
        plugin: Arc<dyn PluginStoreBackend>,
        audit_event: Arc<dyn AuditEventStoreBackend>,
        executor_def: Arc<dyn ExecutorDefStoreBackend>,
        policy_def: Arc<dyn PolicyDefStoreBackend>,
        host: OrbitHostStore,
    ) -> Self {
        Self {
            task,
            task_document,
            task_history,
            task_artifact,
            lexical_index,
            task_reservation,
            job_run,
            tool,
            plugin,
            audit_event,
            executor_def,
            policy_def,
            host,
        }
    }

    pub fn tasks(&self) -> &dyn TaskStoreBackend {
        self.task.as_ref()
    }

    pub fn task_documents(&self) -> &dyn TaskDocumentStoreBackend {
        self.task_document.as_ref()
    }

    pub fn task_history(&self) -> &dyn TaskHistoryStoreBackend {
        self.task_history.as_ref()
    }

    pub fn task_artifacts(&self) -> &dyn TaskArtifactStoreBackend {
        self.task_artifact.as_ref()
    }

    /// The optional lexical index. Callers reach its store through
    /// [`LexicalIndex::store`], which names the absence when there is none.
    pub fn lexical_index(&self) -> &LexicalIndex {
        &self.lexical_index
    }

    pub fn task_reservations(&self) -> &dyn TaskReservationStoreBackend {
        self.task_reservation.as_ref()
    }

    pub fn jobs(&self) -> &dyn JobRunStoreBackend {
        self.job_run.as_ref()
    }

    pub fn tools(&self) -> &dyn ToolStoreBackend {
        self.tool.as_ref()
    }

    pub fn plugins(&self) -> &dyn PluginStoreBackend {
        self.plugin.as_ref()
    }

    pub fn audit_events(&self) -> &dyn AuditEventStoreBackend {
        self.audit_event.as_ref()
    }

    pub fn executors(&self) -> &dyn ExecutorDefStoreBackend {
        self.executor_def.as_ref()
    }

    pub(crate) fn policies(&self) -> &dyn PolicyDefStoreBackend {
        self.policy_def.as_ref()
    }
}

#[derive(Clone)]
pub(crate) struct OrbitExecutionAssets {
    pub(super) registry: Arc<ToolRegistry>,
    pub(super) skill_catalog: SkillCatalog,
    /// What the host plugin load pass registered and refused, kept so
    /// `orbit plugin list`/`show`/`doctor` report the same facts the tool
    /// surface was built from rather than re-deriving them.
    pub(super) plugins: Arc<PluginHostLoad>,
}

impl OrbitExecutionAssets {
    pub(crate) fn new(
        registry: Arc<ToolRegistry>,
        skill_catalog: SkillCatalog,
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
    pub(super) policy: PolicyEngine,
    pub(super) execution_env_policy: ExecutionEnvPolicy,
    pub(super) codex_execution_policy: CodexExecutionPolicy,
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
