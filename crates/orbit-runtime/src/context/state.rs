use super::{
    ActorIdentity, OrbitExecutionAssets, OrbitPolicyContext, OrbitRuntimeSettings, OrbitStores,
};
use crate::plugin::host::PluginHostLoad;
use orbit_config::{CodexExecutionPolicy, ExecutionEnvPolicy, PersistenceConfig};
use orbit_policy::PolicyEngine;
use orbit_store::skill_store::SkillCatalog;
use orbit_tools::ToolRegistry;
use orbit_types::workspace::WorkspacePaths;
use std::path::Path;

#[derive(Clone)]
pub struct OrbitContext {
    paths: WorkspacePaths,
    stores: OrbitStores,
    execution: OrbitExecutionAssets,
    policy: OrbitPolicyContext,
    runtime: OrbitRuntimeSettings,
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
    pub fn data_root(&self) -> &Path {
        self.shared_root()
    }

    pub(crate) fn global_root(&self) -> &Path {
        &self.paths.global_dir
    }

    pub fn paths(&self) -> &WorkspacePaths {
        &self.paths
    }

    pub(crate) fn set_execution_location(
        &mut self,
        location: Option<orbit_types::task::ExecutionLocation>,
    ) {
        self.stores.job_run = self.stores.job_run.with_execution_location(location);
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

    pub(crate) fn plugin_load(&self) -> &PluginHostLoad {
        &self.execution.plugins
    }

    pub(crate) fn execution_env_policy(&self) -> &ExecutionEnvPolicy {
        &self.policy.execution_env_policy
    }

    pub fn apply_child_env_defaults(&mut self, defaults: &[(String, String)]) {
        self.policy.execution_env_policy = self
            .policy
            .execution_env_policy
            .clone()
            .with_defaults(defaults);
    }

    pub(crate) fn codex_execution_policy(&self) -> &CodexExecutionPolicy {
        &self.policy.codex_execution_policy
    }

    pub fn persistence(&self) -> &PersistenceConfig {
        &self.runtime.persistence
    }

    pub(crate) fn actor(&self) -> &ActorIdentity {
        &self.runtime.actor
    }

    pub(crate) fn set_actor(&mut self, actor: ActorIdentity) {
        self.runtime.actor = actor;
    }

    pub(crate) fn set_host_shutdown_hold_minutes(&mut self, minutes: u32) {
        self.runtime.set_host_shutdown_hold_minutes(minutes);
    }

    pub(crate) fn set_worktree_parked_reclaim_after_hours(&mut self, hours: u64) {
        self.runtime.set_worktree_parked_reclaim_after_hours(hours);
    }

    pub(crate) fn set_auto_concurrency_head_guard(&mut self, guard: (u32, u32)) {
        self.runtime.set_auto_concurrency_head_guard(guard);
    }

    pub(crate) fn set_machine_labels(&mut self, labels: Vec<String>) {
        self.runtime.set_machine_labels(labels);
    }

    pub fn settings(&self) -> &OrbitRuntimeSettings {
        &self.runtime
    }
}
