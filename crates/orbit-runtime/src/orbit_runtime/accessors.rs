//! Internal runtime accessors and executor/policy definitions.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use orbit_common::OrbitError;
use orbit_types::workspace::WorkspacePaths;

use super::RuntimeKernel;
use crate::base::Base;
use crate::context::{ActorIdentity, OrbitRuntimeSettings, OrbitStores};
use crate::host::Host;
use crate::run::Run;
use crate::stored::Stored;

/// The four layers. Each is reached by its accessor; none dereferences to
/// another.
impl RuntimeKernel {
    /// The top layer: the run this process carries.
    pub fn run(&self) -> &Run {
        &self.run
    }

    /// This machine as a place work runs.
    pub fn host(&self) -> &Host {
        self.run.host()
    }

    /// The opened stores.
    pub fn stored(&self) -> &Stored {
        self.run.stored()
    }

    /// What every operation has.
    pub fn base(&self) -> &Base {
        self.run.base()
    }

    pub(crate) fn run_mut(&mut self) -> &mut Run {
        &mut self.run
    }

    pub(crate) fn host_mut(&mut self) -> &mut Host {
        self.run.host_mut()
    }

    pub(crate) fn stored_mut(&mut self) -> &mut Stored {
        self.run.host_mut().stored_mut()
    }

    pub(crate) fn base_mut(&mut self) -> &mut Base {
        self.run.host_mut().stored_mut().base_mut()
    }
}

/// Compatibility accessors for what callers outside this crate used to read
/// as public kernel fields. Each returns the same value with the same borrow.
impl RuntimeKernel {
    /// The admitted settings, formerly read as `context.settings()`.
    pub fn settings(&self) -> &OrbitRuntimeSettings {
        self.base().settings()
    }

    /// Where this workspace's state is persisted, formerly read as
    /// `context.persistence()`.
    pub fn persistence(&self) -> &orbit_config::PersistenceConfig {
        self.base().persistence()
    }

    /// Layer `defaults` under the child environment policy, formerly called
    /// as `context.apply_child_env_defaults(..)`.
    pub fn apply_child_env_defaults(&mut self, defaults: &[(String, String)]) {
        self.base_mut().apply_child_env_defaults(defaults);
    }

    /// The in-process session event log.
    pub fn event_log(&self) -> &crate::event_bus::EventLog {
        &self.base().event_log
    }

    /// The observation clock override for deterministic CI-failure filing.
    pub fn ci_failure_time(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        self.base().ci_failure_time
    }

    /// The shared base-run budget of one owner claim settlement.
    pub fn claim_known_red_budget(&self) -> std::time::Duration {
        self.host().claim_known_red_budget
    }

    /// The last tree each run passed full required validation on, by run id.
    /// Held in this process and shared by clones.
    pub fn validated_trees(&self) -> &Arc<Mutex<BTreeMap<String, orbit_engine::ValidatedTree>>> {
        &self.host().validated_trees
    }

    /// When each pull request was last checked for an out-of-run repair, by
    /// pull request number. Held in this process and shared by clones.
    pub fn pr_repair_checks(&self) -> &Arc<Mutex<BTreeMap<u64, std::time::Instant>>> {
        &self.host().pr_repair_checks
    }
}

impl RuntimeKernel {
    pub fn actor(&self) -> &ActorIdentity {
        self.base().actor()
    }

    pub fn actor_label(&self) -> &str {
        self.base().actor().label.as_str()
    }

    pub fn policy_engine(&self) -> &orbit_policy::PolicyEngine {
        self.base().policy()
    }

    /// The immutable environment supplied by composition.
    pub fn env(&self) -> &orbit_common::Env {
        self.base().execution_env_policy().process_env()
    }

    pub fn tool_registry(&self) -> &orbit_tools::ToolRegistry {
        self.host().registry()
    }

    pub fn stores(&self) -> &OrbitStores {
        self.stored().stores()
    }

    /// What the host plugin load pass registered and refused when this
    /// runtime was built.
    pub fn plugin_load(&self) -> &crate::plugin::host::PluginHostLoad {
        self.host().plugin_load()
    }

    pub fn skill_catalog(&self) -> &orbit_store::FileStore<orbit_store::skill_store::SkillDirs> {
        self.host().skill_catalog()
    }

    /// Resolved workspace paths. `pub` for the command surfaces extracted to
    /// `orbit-cmd` [ORB-10016].
    pub fn paths(&self) -> &WorkspacePaths {
        self.base().paths()
    }

    pub fn data_root_path(&self) -> &Path {
        self.shared_root_path()
    }

    /// Whether the composition layer opened this runtime for a workspace the
    /// caller named explicitly, rather than one discovered from the cwd.
    pub fn workspace_explicitly_selected(&self) -> bool {
        self.base().workspace_explicitly_selected
    }

    /// Record that this runtime's workspace was explicitly selected. Only
    /// the composition layer that resolved a selector may set this.
    pub fn with_workspace_explicitly_selected(mut self, selected: bool) -> Self {
        self.base_mut().workspace_explicitly_selected = selected;
        self
    }

    pub(crate) fn shared_root_path(&self) -> &Path {
        self.base().shared_root()
    }

    pub fn execution_env_policy(&self) -> &orbit_config::ExecutionEnvPolicy {
        self.base().execution_env_policy()
    }

    pub fn codex_execution_policy(&self) -> &orbit_config::CodexExecutionPolicy {
        self.base().codex_execution_policy()
    }

    pub fn list_executor_defs(
        &self,
    ) -> Result<Vec<orbit_types::workflow::ExecutorDef>, OrbitError> {
        self.stores().executors().read().executor_def().list()
    }

    pub fn get_executor_def(
        &self,
        name: &str,
    ) -> Result<Option<orbit_types::workflow::ExecutorDef>, OrbitError> {
        self.stores().executors().read().executor_def().get(name)
    }

    /// Where a dispatch from this workspace would launch an executor's
    /// `program` from, or `None` when it would fail to find it. Uses the same
    /// lookup as dispatch (`PATH`, conventional home bins, system prefixes).
    pub fn locate_provider_launcher(&self, program: &str) -> Option<std::path::PathBuf> {
        orbit_engine::activity_job::cli_runner::locate_provider_launcher(
            self.env(),
            program,
            Some(&self.paths().repo_root),
        )
    }

    pub fn upsert_executor_def(
        &self,
        def: &orbit_types::workflow::ExecutorDef,
    ) -> Result<(), OrbitError> {
        self.stores()
            .executors()
            .write(|tx| tx.executor_def().put(def))
    }

    pub fn list_policy_defs(&self) -> Result<Vec<orbit_types::policy::PolicyDef>, OrbitError> {
        self.stores().policies().read().policy_def().list()
    }

    pub fn get_policy_def(
        &self,
        name: &str,
    ) -> Result<Option<orbit_types::policy::PolicyDef>, OrbitError> {
        self.stores().policies().read().policy_def().get(name)
    }

    pub fn upsert_policy_def(
        &self,
        def: &orbit_types::policy::PolicyDef,
    ) -> Result<(), OrbitError> {
        self.stores()
            .policies()
            .write(|tx| tx.policy_def().put(def))
    }
}
