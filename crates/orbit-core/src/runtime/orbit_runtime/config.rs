//! Workflow, validation, and operation configuration.

use super::OrbitRuntime;

impl OrbitRuntime {
    /// Minutes a deferred delivery-automation reason may persist before the
    /// evaluator escalates it to a warning and one friction record.
    pub fn automation_stall_window_minutes(&self) -> u32 {
        self.context.settings().automation_stall_window_minutes()
    }

    pub fn pr_config(&self) -> &orbit_engine::PrConfig {
        self.context.settings().pr_config()
    }

    /// Config-only `[workflow] base_branch` fallback (default `"main"`).
    /// Delivery defaults must use [`Self::workspace_base_branch`], which
    /// prefers the registered workspace base branch when a binding exists.
    pub fn workflow_base_branch(&self) -> &str {
        self.context.settings().workflow_base_branch()
    }

    /// Commands every delivered candidate must pass on its exact commit
    /// (`[workflow] required_validation_commands`): the owner's own delivery
    /// runs them, and an owner accepts a distributed claim's handoff only with
    /// a passing log for each. Empty means no required check on any path: no
    /// command runs and a handoff carries no validation logs.
    pub fn workflow_required_validation_commands(&self) -> &[String] {
        self.context
            .settings()
            .workflow_required_validation_commands()
    }

    /// Commands before-PR review settlement may rerun on the host to confirm
    /// a reviewer's red-base claim (`[review] baseline_commands`), beside
    /// the required validation commands [ORB-14434]. A review admission
    /// captures them, and a failure of one is never a diagnostic [ORB-14684].
    pub fn review_baseline_commands(&self) -> &[String] {
        self.context.settings().review_baseline_commands()
    }

    /// The note a drain submission shows when this host declares no required
    /// validation commands, so "no check" reads as a configured choice rather
    /// than a silent gap. `None` when commands are configured.
    pub fn required_validation_note(&self) -> Option<String> {
        self.workflow_required_validation_commands()
            .is_empty()
            .then(|| {
                "no `workflow.required_validation_commands` configured; delivered candidates \
                 run no required validation here"
                    .to_string()
            })
    }

    /// `[workflow.validation_env]` as admitted [ORB-13987].
    pub fn validation_env_policy(&self) -> &orbit_exec::ValidationEnvPolicy {
        self.context.settings().validation_env()
    }

    /// The environment owner-side repository tooling runs in: the allowlisted
    /// agent environment with PATH and toolchain locators resolved from the
    /// owner user's login shell (cached per process) and
    /// `workflow.validation_env.path`, never from whatever launched this
    /// process [ORB-13987].
    pub fn validation_environment(&self) -> orbit_exec::ValidationEnvironment {
        let base = self.execution_env_policy().agent_subprocess_env(&[]);
        let shell = orbit_exec::LoginShell::for_current_user(&base);
        orbit_exec::ValidationEnvironment::resolve(base, self.validation_env_policy(), &shell)
    }

    /// The environment `proc.spawn` runs a command with: the
    /// `[execution.env]` allowlist plus `extras`, with
    /// `workflow.validation_env.path` ahead of PATH, so a reviewer resolves
    /// the interpreter and tools required validation uses [ORB-15204]. Inside
    /// an agent session the inherited PATH already carries
    /// [`Self::agent_session_environment`]'s entries, so no login shell is
    /// probed per call.
    pub(crate) fn agent_environment(&self, extras: &[&str]) -> Vec<(String, String)> {
        self.validation_env_policy()
            .agent_environment(self.execution_env_policy().agent_subprocess_env(extras))
    }

    /// The environment an agent session starts with, in every lane
    /// (implementer, before-PR and before-landing reviewer): the provider CLI
    /// and the commands its shell runs. It is the `proc.spawn` environment
    /// (`workflow.validation_env.path` first) with every entry of
    /// [`Self::validation_environment`]'s PATH that it lacks appended, so a
    /// session finds each tool required validation finds without a
    /// hand-built PATH wrapper [ORB-15293].
    pub fn agent_session_environment(&self, extras: &[&str]) -> Vec<(String, String)> {
        self.validation_environment()
            .extend_agent_environment(self.agent_environment(extras))
    }

    /// Why required validation may not find the user's toolchain on this
    /// host, for drain submission and `orbit doctor` [ORB-13987]. `None` when
    /// no required commands are configured or the environment resolved
    /// cleanly.
    pub fn validation_env_preflight_warning(&self) -> Option<String> {
        if self.workflow_required_validation_commands().is_empty() {
            return None;
        }
        self.validation_environment().preflight_warning()
    }

    /// The operator-added `execution.env.pass` names this runtime has no
    /// non-empty child-environment value for [ORB-14777], including clock
    /// credentials supplied as runtime defaults. Names only, never values.
    /// Agents this runtime starts, directly or through a detached worker,
    /// do not receive them.
    pub fn unset_env_pass_names(&self) -> Vec<String> {
        self.execution_env_policy().unset_pass_names()
    }

    /// The `execution.env.pass` names this workspace's agents may receive.
    pub fn env_pass_names(&self) -> Vec<String> {
        self.execution_env_policy().pass_names().to_vec()
    }

    /// The warning for [`Self::unset_env_pass_names`], for drain/ship starts
    /// and `orbit doctor`. `None` when every pass-listed variable is set.
    pub fn unset_env_pass_warning(&self) -> Option<String> {
        let unset = self.unset_env_pass_names();
        (!unset.is_empty()).then(|| unset_env_pass_message(&unset))
    }

    /// `[workflow] distributed_completion`: `review` or `done`.
    pub fn workflow_distributed_completion(&self) -> &str {
        self.context.settings().workflow_distributed_completion()
    }

    /// `[workflow.task_pilot_freshness]`, before any routine override.
    pub(crate) fn task_pilot_freshness(
        &self,
    ) -> &orbit_types::workflow::automation::members::PreparationFreshness {
        self.context.settings().task_pilot_freshness()
    }

    /// The branch this workspace integrates into: the registered workspace
    /// base branch when a registry binding exists, else `[workflow]
    /// base_branch`. Delivery automation defaults are seeded against it.
    pub fn workspace_base_branch(&self) -> &str {
        self.workspace_runtime_binding()
            .and_then(|binding| binding.base_branch.as_deref())
            .unwrap_or_else(|| self.workflow_base_branch())
    }

    /// Whether this workspace opted into unattended ship dispatch
    /// (`[workflow] auto_ship` in the active `config.toml`; defaults to
    /// `false`). Consulted by `orbit run ship-sweep` and other schedulers
    /// before dispatching ship runs nobody explicitly asked for.
    pub fn workflow_auto_ship(&self) -> bool {
        self.context.settings().workflow_auto_ship()
    }

    /// The resolved `[operation]` review preferences (layered over the
    /// built-in no-review defaults) with per-field provenance [ORB-11333].
    pub fn operation_policy(&self) -> &orbit_config::OperationPolicy {
        self.context.settings().operation()
    }
}

/// The shared wording for pass-listed variables missing from the launching
/// environment [ORB-14777].
fn unset_env_pass_message(unset: &[String]) -> String {
    format!(
        "`execution.env.pass` names {} unset or empty in this environment, so agents will not \
         receive {}; a provider that needs one falls back to another login. Drains inherit the \
         launching shell's environment: start them from a login shell",
        unset.join(", "),
        if unset.len() == 1 { "it" } else { "them" },
    )
}
