//! Internal runtime accessors and executor/policy definitions.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::workspace::WorkspacePaths;

use super::OrbitRuntime;
use crate::context::{ActorIdentity, OrbitStores};

impl OrbitRuntime {
    pub(crate) fn actor(&self) -> &ActorIdentity {
        self.context.actor()
    }

    pub(crate) fn actor_label(&self) -> &str {
        self.context.actor().label.as_str()
    }

    pub(crate) fn policy_engine(&self) -> &orbit_policy::PolicyEngine {
        self.context.policy()
    }

    pub(crate) fn tool_registry(&self) -> &orbit_tools::ToolRegistry {
        self.context.registry()
    }

    pub(crate) fn stores(&self) -> &OrbitStores {
        self.context.stores()
    }

    /// What the host plugin load pass registered and refused when this
    /// runtime was built.
    pub(crate) fn plugin_load(&self) -> &crate::runtime::plugin::host::PluginHostLoad {
        self.context.plugin_load()
    }

    pub(crate) fn skill_catalog(&self) -> &crate::skill_catalog::SkillCatalog {
        self.context.skill_catalog()
    }

    /// Resolved workspace paths. `pub` for the command surfaces extracted to
    /// `orbit-cmd` [ORB-10016].
    pub fn paths(&self) -> &WorkspacePaths {
        self.context.paths()
    }

    pub(crate) fn data_root_path(&self) -> &Path {
        self.shared_root_path()
    }

    pub(crate) fn shared_root_path(&self) -> &Path {
        self.context.shared_root()
    }

    pub(crate) fn execution_env_policy(&self) -> &orbit_config::ExecutionEnvPolicy {
        self.context.execution_env_policy()
    }

    pub(crate) fn codex_execution_policy(&self) -> &orbit_config::CodexExecutionPolicy {
        self.context.codex_execution_policy()
    }

    /// `execution.proc_spawn_max_timeout_minutes`, in milliseconds.
    pub(crate) fn proc_spawn_max_timeout_ms(&self) -> u64 {
        self.context.proc_spawn_max_timeout_ms()
    }

    pub fn list_executor_defs(
        &self,
    ) -> Result<Vec<orbit_types::workflow::ExecutorDef>, OrbitError> {
        self.stores().executors().list_executor_defs()
    }

    pub fn get_executor_def(
        &self,
        name: &str,
    ) -> Result<Option<orbit_types::workflow::ExecutorDef>, OrbitError> {
        self.stores().executors().get_executor_def(name)
    }

    /// Where a dispatch from this workspace would launch an executor's
    /// `program` from, or `None` when it would fail to find it. Uses the same
    /// lookup as dispatch (`PATH`, conventional home bins, system prefixes).
    pub fn locate_provider_launcher(&self, program: &str) -> Option<std::path::PathBuf> {
        orbit_engine::activity_job::cli_runner::locate_provider_launcher(
            program,
            Some(&self.paths().repo_root),
        )
    }

    pub fn upsert_executor_def(
        &self,
        def: &orbit_types::workflow::ExecutorDef,
    ) -> Result<(), OrbitError> {
        self.stores().executors().upsert_executor_def(def)
    }

    pub fn list_policy_defs(&self) -> Result<Vec<orbit_types::policy::PolicyDef>, OrbitError> {
        self.stores().policies().list_policy_defs()
    }

    pub fn get_policy_def(
        &self,
        name: &str,
    ) -> Result<Option<orbit_types::policy::PolicyDef>, OrbitError> {
        self.stores().policies().get_policy_def(name)
    }

    pub fn upsert_policy_def(
        &self,
        def: &orbit_types::policy::PolicyDef,
    ) -> Result<(), OrbitError> {
        self.stores().policies().upsert_policy_def(def)
    }
}
