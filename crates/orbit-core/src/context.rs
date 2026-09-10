use std::path::Path;
use std::sync::Arc;

use orbit_engine::PrConfig;
use orbit_policy::PolicyEngine;
use orbit_search::{EmbedWorker, SemanticIndex};
use orbit_store::Store;
use orbit_store::contracts::{
    AuditEventStoreBackend, AutomationStoreBackend, ExecutorDefStoreBackend,
    InvocationStoreBackend, JobRunStoreBackend, OperationStoreBackend, PolicyDefStoreBackend,
    ReviewStoreBackend, TaskArtifactStoreBackend, TaskDocumentStoreBackend,
    TaskHistoryStoreBackend, TaskReservationStoreBackend, TaskStoreBackend, ToolStoreBackend,
    V2AuditStoreBackend,
};
use orbit_tools::ToolRegistry;
use orbit_types::identity::{Crew, normalize_agent_family_for_model};
use orbit_types::workspace::WorkspacePaths;

use crate::skill_catalog::SkillCatalog;
use orbit_config::{CodexExecutionPolicy, ExecutionEnvPolicy, PersistenceConfig};

const ORBIT_AGENT_NAME: &str = "ORBIT_AGENT_NAME";
const ORBIT_AGENT_MODEL: &str = "ORBIT_AGENT_MODEL";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorKind {
    Unknown,
    Human,
    Agent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActorIdentity {
    pub kind: ActorKind,
    pub label: String,
}

impl ActorIdentity {
    pub fn unknown() -> Self {
        Self {
            kind: ActorKind::Unknown,
            label: "unknown".to_string(),
        }
    }

    pub fn human(label: impl Into<String>) -> Self {
        Self {
            kind: ActorKind::Human,
            label: normalize_actor_label(label.into(), "human"),
        }
    }

    pub fn agent(label: impl Into<String>) -> Self {
        Self {
            kind: ActorKind::Agent,
            label: normalize_actor_label(label.into(), "agent"),
        }
    }

    /// Resolve the self-reported process identity used by CLI and dashboard
    /// entry points.
    ///
    /// The environment is not an authentication boundary. Agent values are
    /// therefore reduced to the same canonical family used by tool dispatch,
    /// and an absent or inconsistent envelope is recorded as `unknown` rather
    /// than claiming that a human was present.
    pub fn from_env() -> Self {
        let agent = std::env::var(ORBIT_AGENT_NAME)
            .ok()
            .filter(|value| !value.trim().is_empty());
        let model = std::env::var(ORBIT_AGENT_MODEL)
            .ok()
            .filter(|value| !value.trim().is_empty());

        normalize_agent_family_for_model(agent.as_deref(), model.as_deref())
            .ok()
            .flatten()
            .map(Self::agent)
            .unwrap_or_default()
    }
}

impl Default for ActorIdentity {
    fn default() -> Self {
        Self::unknown()
    }
}

#[derive(Clone)]
pub struct OrbitContext {
    paths: WorkspacePaths,
    stores: OrbitStores,
    execution: OrbitExecutionAssets,
    policy: OrbitPolicyContext,
    runtime: OrbitRuntimeSettings,
}

/// Host SQLite handle plus the feature backends composed from it.
///
/// These wrap the same writer connection the runtime opened at construction.
/// Accessors clone the handles; they must not call `Store::open` again.
#[derive(Clone)]
pub(crate) struct OrbitHostStore {
    pub(crate) sqlite: Store,
    pub(crate) automation: Arc<dyn AutomationStoreBackend>,
    pub(crate) review: Arc<dyn ReviewStoreBackend>,
    pub(crate) operation: Arc<dyn OperationStoreBackend>,
    pub(crate) v2_audit: Arc<dyn V2AuditStoreBackend>,
    pub(crate) invocation: Arc<dyn InvocationStoreBackend>,
}

#[derive(Clone)]
pub(crate) struct OrbitStores {
    pub(crate) task: Arc<dyn TaskStoreBackend>,
    pub(crate) task_document: Arc<dyn TaskDocumentStoreBackend>,
    pub(crate) task_history: Arc<dyn TaskHistoryStoreBackend>,
    pub(crate) task_artifact: Arc<dyn TaskArtifactStoreBackend>,
    pub(crate) semantic_index: SemanticIndex,
    pub(crate) semantic_worker: Arc<EmbedWorker>,
    pub(crate) task_reservation: Arc<dyn TaskReservationStoreBackend>,
    pub(crate) job_run: Arc<dyn JobRunStoreBackend>,
    pub(crate) tool: Arc<dyn ToolStoreBackend>,
    pub(crate) audit_event: Arc<dyn AuditEventStoreBackend>,
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
        semantic_index: SemanticIndex,
        semantic_worker: Arc<EmbedWorker>,
        task_reservation: Arc<dyn TaskReservationStoreBackend>,
        job_run: Arc<dyn JobRunStoreBackend>,
        tool: Arc<dyn ToolStoreBackend>,
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
            semantic_index,
            semantic_worker,
            task_reservation,
            job_run,
            tool,
            audit_event,
            executor_def,
            policy_def,
            host,
        }
    }

    pub(crate) fn tasks(&self) -> &dyn TaskStoreBackend {
        self.task.as_ref()
    }

    pub(crate) fn task_documents(&self) -> &dyn TaskDocumentStoreBackend {
        self.task_document.as_ref()
    }

    pub(crate) fn task_history(&self) -> &dyn TaskHistoryStoreBackend {
        self.task_history.as_ref()
    }

    pub(crate) fn task_artifacts(&self) -> &dyn TaskArtifactStoreBackend {
        self.task_artifact.as_ref()
    }

    /// The optional semantic index. Callers reach its store through
    /// [`SemanticIndex::store`], which names the absence when there is none.
    pub(crate) fn semantic_index(&self) -> &SemanticIndex {
        &self.semantic_index
    }

    pub(crate) fn semantic_worker(&self) -> &EmbedWorker {
        self.semantic_worker.as_ref()
    }

    pub(crate) fn task_reservations(&self) -> &dyn TaskReservationStoreBackend {
        self.task_reservation.as_ref()
    }

    pub(crate) fn jobs(&self) -> &dyn JobRunStoreBackend {
        self.job_run.as_ref()
    }

    pub(crate) fn tools(&self) -> &dyn ToolStoreBackend {
        self.tool.as_ref()
    }

    pub(crate) fn audit_events(&self) -> &dyn AuditEventStoreBackend {
        self.audit_event.as_ref()
    }

    pub(crate) fn executors(&self) -> &dyn ExecutorDefStoreBackend {
        self.executor_def.as_ref()
    }

    pub(crate) fn policies(&self) -> &dyn PolicyDefStoreBackend {
        self.policy_def.as_ref()
    }
}

#[derive(Clone)]
pub(crate) struct OrbitExecutionAssets {
    registry: Arc<ToolRegistry>,
    skill_catalog: SkillCatalog,
}

impl OrbitExecutionAssets {
    pub(crate) fn new(registry: Arc<ToolRegistry>, skill_catalog: SkillCatalog) -> Self {
        Self {
            registry,
            skill_catalog,
        }
    }
}

#[derive(Clone)]
pub(crate) struct OrbitPolicyContext {
    policy: PolicyEngine,
    execution_env_policy: ExecutionEnvPolicy,
    codex_execution_policy: CodexExecutionPolicy,
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

#[derive(Clone)]
pub(crate) struct OrbitRuntimeSettings {
    persistence: PersistenceConfig,
    actor: ActorIdentity,
    scoring_enabled: bool,
    pr_config: PrConfig,
    /// Default base branch for ship workflows
    /// (`[workflow] base_branch` in `config.toml`, default `"main"`).
    workflow_base_branch: String,
    /// Opt-in for unattended ship dispatch
    /// (`[workflow] auto_ship` in `config.toml`, default `false`).
    workflow_auto_ship: bool,
    /// Whether this workspace is a routine source
    /// (`[routines] role = "source"` in `config.toml`, default `false`).
    routines_source: bool,
    crews: std::collections::BTreeMap<String, Crew>,
    default_crew: Option<String>,
    complexity_crews: orbit_config::ComplexityCrewPools,
    system_crew: String,
    /// Resolved operation-mode preferences with provenance (`[operation]`).
    /// Preferences only; authority is a separate durable grant [ORB-11332].
    operation: orbit_config::OperationPolicy,
}

impl OrbitRuntimeSettings {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        persistence: PersistenceConfig,
        actor: ActorIdentity,
        scoring_enabled: bool,
        pr_config: PrConfig,
        workflow_base_branch: String,
        workflow_auto_ship: bool,
        routines_source: bool,
        crews: std::collections::BTreeMap<String, Crew>,
        default_crew: Option<String>,
        complexity_crews: orbit_config::ComplexityCrewPools,
        system_crew: String,
        operation: orbit_config::OperationPolicy,
    ) -> Self {
        Self {
            persistence,
            actor,
            scoring_enabled,
            pr_config,
            workflow_base_branch,
            workflow_auto_ship,
            routines_source,
            crews,
            default_crew,
            complexity_crews,
            system_crew,
            operation,
        }
    }

    pub(crate) fn operation(&self) -> &orbit_config::OperationPolicy {
        &self.operation
    }

    pub(crate) fn pr_config(&self) -> &PrConfig {
        &self.pr_config
    }

    pub(crate) fn workflow_base_branch(&self) -> &str {
        &self.workflow_base_branch
    }

    pub(crate) fn workflow_auto_ship(&self) -> bool {
        self.workflow_auto_ship
    }

    pub(crate) fn routines_source(&self) -> bool {
        self.routines_source
    }

    pub(crate) fn crews(&self) -> &std::collections::BTreeMap<String, Crew> {
        &self.crews
    }

    pub(crate) fn complexity_crews(&self) -> &orbit_config::ComplexityCrewPools {
        &self.complexity_crews
    }

    pub(crate) fn default_crew(&self) -> Option<&str> {
        self.default_crew.as_deref()
    }

    pub(crate) fn system_crew(&self) -> &str {
        &self.system_crew
    }
}

impl OrbitContext {
    pub(crate) fn new(
        paths: WorkspacePaths,
        stores: OrbitStores,
        execution: OrbitExecutionAssets,
        policy: OrbitPolicyContext,
        runtime: OrbitRuntimeSettings,
    ) -> Self {
        Self {
            paths,
            stores,
            execution,
            policy,
            runtime,
        }
    }

    /// Returns the shared .orbit/ data directory.
    pub(crate) fn shared_root(&self) -> &Path {
        &self.paths.orbit_dir
    }

    /// Returns the worktree-local .orbit/ data directory.
    pub(crate) fn local_root(&self) -> &Path {
        &self.paths.local_dir
    }

    /// Returns the .orbit/ data directory (backward-compatible alias).
    pub(crate) fn data_root(&self) -> &Path {
        self.shared_root()
    }

    pub(crate) fn global_root(&self) -> &Path {
        &self.paths.global_dir
    }

    pub(crate) fn paths(&self) -> &WorkspacePaths {
        &self.paths
    }

    pub(crate) fn stores(&self) -> &OrbitStores {
        &self.stores
    }

    pub(crate) fn policy(&self) -> &PolicyEngine {
        &self.policy.policy
    }

    pub(crate) fn registry(&self) -> &ToolRegistry {
        self.execution.registry.as_ref()
    }

    pub(crate) fn skill_catalog(&self) -> &SkillCatalog {
        &self.execution.skill_catalog
    }

    pub(crate) fn execution_env_policy(&self) -> &ExecutionEnvPolicy {
        &self.policy.execution_env_policy
    }

    pub(crate) fn codex_execution_policy(&self) -> &CodexExecutionPolicy {
        &self.policy.codex_execution_policy
    }

    pub(crate) fn persistence(&self) -> &PersistenceConfig {
        &self.runtime.persistence
    }

    pub(crate) fn actor(&self) -> &ActorIdentity {
        &self.runtime.actor
    }

    pub(crate) fn set_actor(&mut self, actor: ActorIdentity) {
        self.runtime.actor = actor;
    }

    #[cfg(test)]
    pub(crate) fn replace_task_store_for_test(&mut self, task: Arc<dyn TaskStoreBackend>) {
        self.stores.task = task;
    }

    #[cfg(test)]
    pub(crate) fn task_store_for_test(&self) -> Arc<dyn TaskStoreBackend> {
        Arc::clone(&self.stores.task)
    }

    pub(crate) fn scoring_enabled(&self) -> bool {
        self.runtime.scoring_enabled
    }

    pub(crate) fn settings(&self) -> &OrbitRuntimeSettings {
        &self.runtime
    }
}

fn normalize_actor_label(label: String, default_label: &str) -> String {
    let label = label.trim();
    if label.is_empty() {
        default_label.to_string()
    } else {
        label.to_string()
    }
}
