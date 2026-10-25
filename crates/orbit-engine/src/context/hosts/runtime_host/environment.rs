macro_rules! environment_methods {
    () => {
        /// Admit an implementer's criteria owed after deploy against the task
        /// pilot's post-deploy finding, refusing any criterion the pilot did not
        /// type. An owner run records the admitted claim in task history; a
        /// claimed leaf only checks it, because the owner admits it again when
        /// it accepts the handoff.
        fn admit_owed_criteria(
            &self,
            task_id: &str,
            run_id: &str,
            owed: &[orbit_types::workflow::OwedCriterion],
        ) -> Result<(), OrbitError>;

        /// The criteria of the task's current post-deploy record that no
        /// operator has recorded met. With `run_id`, only a record that run made
        /// counts. A delivery that still owes any settles in `review`.
        fn outstanding_owed_criteria(
            &self,
            task_id: &str,
            run_id: Option<&str>,
        ) -> Result<Vec<usize>, OrbitError>;

        // ── Config accessors ───────────────────────────────────────────────

        /// Commands every delivered candidate must pass
        /// (`workflow.required_validation_commands`). Empty means no required
        /// check: the owner delivery path validates nothing.
        fn required_validation_commands(&self) -> Vec<String>;

        /// The owner's review gates (`review.baseline_commands`). With the
        /// required commands they are the gates whose Bubblewrap deferrals an
        /// implementer may hand off for a native replay [ORB-15287].
        fn review_baseline_commands(&self) -> Vec<String>;

        /// The gates this host holds a candidate to, with where it read them
        /// [ORB-15622]. An owner answers with its own accessors; a claimed leaf's
        /// host answers with the owner's gates its claim captured, so a
        /// replica's drifted configuration never decides which deferrals it
        /// accepts or replays.
        fn validation_gates(&self) -> orbit_types::workflow::HostValidationGates;

        /// `workflow.unlocked_context_paths` and
        /// `workflow.unlocked_path_validation_commands`: what a candidate that
        /// changed only unlocked paths since its run's last fully validated tree
        /// runs instead of the required list [ORB-16784].
        fn unlocked_path_validation(&self) -> super::UnlockedPathValidation;

        /// `workflow.generated_paths`: the conflicting paths conflict recovery
        /// regenerates with a command instead of merging.
        fn generated_paths(&self) -> Vec<super::GeneratedPath>;

        /// Remember `tree` as the last tree its run passed the full required
        /// list on. The record is the host's own, never task or run-state data
        /// an agent can write; a host that cannot keep one records nothing, so
        /// every later candidate of the run is validated in full.
        fn record_validated_tree(&self, tree: &super::ValidatedTree) -> Result<(), OrbitError>;

        /// The tree [`Self::record_validated_tree`] last recorded for `run_id`.
        fn validated_tree(&self, run_id: &str) -> Result<Option<super::ValidatedTree>, OrbitError>;

        /// The OS this host runs implementers on, which decides the deferral
        /// kinds it admits [ORB-15587]: this process's OS for a local host.
        fn host_os(&self) -> Option<orbit_types::workflow::EvidenceHostOs>;

        /// Returns provider-agnostic key-value configuration that is forwarded
        /// to the selected provider factory so it can decode any provider-specific
        /// settings (for example Codex reads `"sandbox"`, `"approval_policy"` and
        /// `"allow_login_shell"`).
        fn agent_provider_config(&self) -> HashMap<String, String>;
        /// How long a provider process may produce no output and use no CPU
        /// before its step fails as stalled. `None` disables the check and leaves
        /// only the wall-clock deadline. The built-in window is 30 minutes.
        fn agent_stall_window(&self) -> Option<std::time::Duration>;
        /// The complete environment an agent subprocess is launched with.
        ///
        /// Launchers clear the child environment and apply exactly what this
        /// returns, so anything absent here does not reach the provider.
        /// `required_env_vars` are the names the provider runtime declares it
        /// needs. A host with no configuration answers the built-in baseline
        /// plus those extras, so it still starts a provider but never forwards
        /// ambient credentials. `OrbitRuntime` answers with the operator's
        /// `[execution.env]` policy, with `workflow.validation_env.path` ahead of
        /// PATH and the rest of the resolved validation PATH after it, the same
        /// for every agent lane. [ORB-10917] [ORB-15204] [ORB-15293]
        fn agent_subprocess_environment(&self, required_env_vars: &[&str])
        -> Vec<(String, String)>;
        /// [`Self::agent_subprocess_environment`] for an agent of a crew with
        /// `[crews.<name>] env` entries: each entry's name gets its source's value
        /// from the environment the host launched with (runtime defaults such as
        /// `clock.env` included) and wins over `pass`; an entry whose source is
        /// unset is absent; a crew that sets one Claude credential has the other
        /// removed. With no entries it answers exactly what
        /// `agent_subprocess_environment` does.
        fn crew_agent_subprocess_environment(
            &self,
            crew_env: &[orbit_types::identity::CrewEnvVar],
            required_env_vars: &[&str],
        ) -> Vec<(String, String)>;
        /// Whether a Claude activity must be launched with its own credential
        /// (`CLAUDE_CODE_OAUTH_TOKEN` or `ANTHROPIC_API_KEY`) rather than start on
        /// the login the Claude Desktop app shares [ORB-15154].
        ///
        /// The Desktop revokes that shared login when it refreshes, so an
        /// unattended run that starts on it fails with a 401 mid-way. A host
        /// that owns no such login answers `false`; `OrbitRuntime` answers `true`
        /// on macOS.
        fn requires_claude_worker_credential(&self) -> bool;
        /// The environment owner-side repository tooling runs in: required
        /// validation and `local_shell` steps [ORB-13987].
        ///
        /// It starts from the agent subprocess environment, so the allowlist still
        /// decides every variable, but PATH and toolchain locators must not depend
        /// on whatever launched the worker. A host without that configuration
        /// keeps the launcher's PATH (`launcher_fallback`); `OrbitRuntime` resolves the owner user's login
        /// shell under `[workflow.validation_env]`, including workspace-selected
        /// build output bindings applied by host validation runners.
        fn validation_subprocess_environment(&self) -> orbit_exec::ValidationEnvironment;
        /// Workspace output bindings for cache identity. Configured hosts override
        /// this with a config-only read so observational cache queries do not probe
        /// a login shell. The default shares the host's environment contract.
        fn validation_build_output_env(&self) -> Vec<String> {
            self.validation_subprocess_environment().build_output_env
        }

        /// The authoritative shared Orbit registry root to hand a spawned CLI
        /// agent as `ORBIT_REGISTRY_ROOT`.
        ///
        /// This is the registry that owns the task store, not the dispatching
        /// checkout's workspace `.orbit`. The locator selects only the global
        /// registry. Workspace ownership for nested tool calls is the separate
        /// logical selector from [`RuntimeHost::orbit_workspace_selector`].
        /// [ORB-10980] [ORB-11066] [ORB-11117]
        fn orbit_registry_root(&self) -> Option<String>;
        /// The trusted logical `ws_*` selector to hand a spawned CLI agent as
        /// `ORBIT_WORKSPACE`.
        ///
        /// Nested MCP and CLI tool calls use this identity instead of inferring
        /// durable ownership from a linked-worktree cwd. Hosts that are not bound
        /// to a registered workspace return `None`, and the child then keeps its
        /// ordinary fail-closed resolution. [ORB-11117]
        fn orbit_workspace_selector(&self) -> Option<String>;
        /// Rebind durable runtime handles after an external CLI provider exits.
        ///
        /// Provider sandboxes may receive explicit access to a SQLite database and
        /// its WAL sidecars. A host with cached connections refreshes them here so
        /// completion audit and checkpoints target the authoritative files.
        fn refresh_persistence_after_cli_provider(&self) -> Result<(), OrbitError>;

        // ── Runtime services ───────────────────────────────────────────────

        fn record_event(&self, event: OrbitEvent) -> Result<(), OrbitError>;
        fn repo_root(&self) -> Result<String, OrbitError>;
        fn list_job_runs_for_gc(&self) -> Result<Vec<JobRun>, OrbitError>;
        /// Admitted rebuildable paths, independent of checkout-authored commands.
        fn worktree_reclaim_patterns(&self) -> Vec<String>;
        /// `worktree.parked_reclaim_after_hours`: how long a task must have been
        /// parked in `someday` before the scheduled sweep may remove its run's
        /// worktree. `None` keeps parked worktrees.
        fn worktree_parked_reclaim_after_hours(&self) -> Option<u64>;
        /// A task's settlement state for worktree GC, read from the store that
        /// owns the workspace's tasks. `run_id` is the run whose worktree is
        /// being classified, so a replica can ask through that run's own claim
        /// route. An owner reads its own store; a replica host asks its owner.
        fn lookup_task_for_worktree_gc(&self, run_id: &str, task_id: &str) -> WorktreeGcTaskLookup;
        /// Stable scope for memoizing task lookups during one GC sweep. Replica
        /// hosts return the claim's owner selector, because one checkout may hold
        /// claims routed to different owners. `None` disables memoization.
        fn worktree_gc_task_lookup_scope(&self, run_id: &str) -> Option<String>;
        /// Whether `run_id` is a claimed leaf whose claim this follower has
        /// settled with its owner. The owner then holds the leaf's delivery, so
        /// a terminal leaf's worktree is this machine's to reclaim without asking
        /// about the task. `Some` names the settlement for the GC report; a host
        /// that pulls no work holds no claims.
        fn settled_claim_for_worktree_gc(&self, run_id: &str) -> Option<String>;
        /// Whether `run_id` is a claimed leaf whose claim is not yet settled with
        /// its owner, so this follower may still owe the owner the leaf's
        /// delivery. `Some` names the claim's phase, or why it could not be read.
        fn live_claim_for_worktree_gc(&self, run_id: &str) -> Option<String>;
        /// Prune this checkout's `.orbit/tmp` of top-level entries untouched for
        /// `retention_hours`. `None` means the host keeps no scratch directory to
        /// sweep.
        fn gc_scratch(&self, retention_hours: u64) -> Result<Option<ScratchGcReport>, OrbitError>;
        fn data_root(&self) -> &Path;
        fn cancel_job_run(&self, run_id: &str) -> Result<(), OrbitError>;
        fn invocation_records(
            &self,
            query: InvocationQuery,
        ) -> Result<Vec<InvocationRecord>, OrbitError>;
        /// [ORB-14695] Record a provider usage limit a run observed in this
        /// host's provider-limit store. The latest observation per provider,
        /// model scope and window stands; an older one never replaces it.
        fn record_provider_limit(
            &self,
            observation: &ProviderLimitObservation,
        ) -> Result<(), OrbitError>;
        fn activity_implementer_identity(
            &self,
            input: &Value,
        ) -> Result<(Option<String>, Option<String>), OrbitError>;
        /// Return the exact model string persisted when the run's crew was
        /// resolved. Workflow commit attribution treats this as opaque config
        /// data and falls back when the run or model is unavailable.
        fn resolved_crew_model(&self, run_id: &str) -> Result<Option<String>, OrbitError>;
        fn run_tool_with_context_and_role(
            &self,
            name: &str,
            input: Value,
            role: Role,
            tool_context: ToolContext,
        ) -> Result<Value, OrbitError>;
        /// Execute an engine-private VCS/PR operation for deterministic shipment.
        ///
        /// Unlike `run_tool_with_context_and_role`, this boundary never consults
        /// the public Tool registry, public authorization, or an activity
        /// allowlist. `OrbitRuntime` answers with
        /// [`crate::run_private_vcs_operation`]; tests may substitute an
        /// in-memory fake.
        fn run_private_vcs_operation(
            &self,
            operation: &str,
            input: Value,
        ) -> Result<Value, OrbitError>;
        fn resolved_agent_model_pair(&self, agent_cli: &str) -> Option<AgentModelPair>;
        /// Return the current agent model identity when this runtime is operating
        /// as an agent, or `None` when there is no model-bearing actor.
        fn actor_model_identity(&self) -> Option<String>;
        fn pr_config(&self) -> PrConfig;
    };
}
pub(super) use environment_methods;
