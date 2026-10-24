//! Internal runtime accessors and executor/policy definitions.

use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::workspace::WorkspacePaths;

use super::RuntimeKernel;
use crate::context::{ActorIdentity, OrbitStores};

impl RuntimeKernel {
    pub fn actor(&self) -> &ActorIdentity {
        self.context.actor()
    }

    pub fn actor_label(&self) -> &str {
        self.context.actor().label.as_str()
    }

    pub fn policy_engine(&self) -> &orbit_policy::PolicyEngine {
        self.context.policy()
    }

    pub fn tool_registry(&self) -> &orbit_tools::ToolRegistry {
        self.context.registry()
    }

    pub fn stores(&self) -> &OrbitStores {
        self.context.stores()
    }

    /// What the host plugin load pass registered and refused when this
    /// runtime was built.
    pub fn plugin_load(&self) -> &crate::plugin::host::PluginHostLoad {
        self.context.plugin_load()
    }

    pub fn skill_catalog(&self) -> &orbit_store::skill_store::SkillCatalog {
        self.context.skill_catalog()
    }

    /// Resolved workspace paths. `pub` for the command surfaces extracted to
    /// `orbit-cmd` [ORB-10016].
    pub fn paths(&self) -> &WorkspacePaths {
        self.context.paths()
    }

    pub fn data_root_path(&self) -> &Path {
        self.shared_root_path()
    }

    pub(crate) fn shared_root_path(&self) -> &Path {
        self.context.shared_root()
    }

    pub fn execution_env_policy(&self) -> &orbit_config::ExecutionEnvPolicy {
        self.context.execution_env_policy()
    }

    pub fn codex_execution_policy(&self) -> &orbit_config::CodexExecutionPolicy {
        self.context.codex_execution_policy()
    }

    /// `execution.proc_spawn_max_timeout_minutes`, in milliseconds.
    pub fn proc_spawn_max_timeout_ms(&self) -> u64 {
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
