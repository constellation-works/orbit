//! Base-layer accessors on [`RuntimeKernel`].

use std::path::Path;

use orbit_types::workspace::WorkspacePaths;

use super::actor::ActorIdentity;
use super::config::OrbitRuntimeSettings;
use super::event::EventLog;
use crate::RuntimeKernel;

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
    pub fn event_log(&self) -> &EventLog {
        &self.base().event_log
    }

    /// The observation clock override for deterministic CI-failure filing.
    pub fn ci_failure_time(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        self.base().ci_failure_time
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
}
