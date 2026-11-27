use std::collections::HashMap;

use orbit_common::OrbitError;
use orbit_store::contract::{InvocationQuery, InvocationRecord};
use orbit_tools::ToolContext;
use orbit_types::identity::AgentModelPair;
use orbit_types::record::OrbitEvent;
use orbit_types::telemetry::ProviderLimitObservation;
use orbit_types::workflow::JobRun;
use serde_json::Value;

use crate::context::RunSettings;
use crate::context::hosts::{ScratchGcReport, ValidatedTree, WorktreeGcTaskLookup};

/// Host configuration and environment: resolved run settings, owed criteria,
/// subprocess environments, events, worktree and scratch GC, provider
/// telemetry, tool runs and model identity.
pub trait HostEnvironment: Send + Sync {
    /// Resolved host and workspace configuration for this run, delivered as one
    /// value rather than individual configuration getters.
    fn run_settings(&self) -> RunSettings;

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

    /// Remember `tree` as the last tree its run passed the full required
    /// list on. The record is the host's own, never task or run-state data
    /// an agent can write; a host that cannot keep one records nothing, so
    /// every later candidate of the run is validated in full.
    fn record_validated_tree(&self, tree: &ValidatedTree) -> Result<(), OrbitError>;

    /// The tree [`Self::record_validated_tree`] last recorded for `run_id`.
    fn validated_tree(&self, run_id: &str) -> Result<Option<ValidatedTree>, OrbitError>;

    /// Returns provider-agnostic key-value configuration that is forwarded
    /// to the selected provider factory so it can decode any provider-specific
    /// settings (for example Codex reads `"sandbox"`, `"approval_policy"` and
    /// `"allow_login_shell"`).
    fn agent_provider_config(&self) -> HashMap<String, String>;

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
    fn agent_subprocess_environment(&self, required_env_vars: &[&str]) -> Vec<(String, String)>;

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

    /// The environment owner-side repository tooling runs in: required
    /// validation and `local_shell` steps [ORB-13987].
    ///
    /// It starts from the agent subprocess environment, so the allowlist still
    /// decides every variable, but PATH and toolchain locators must not depend
    /// on whatever launched the worker. A host without that configuration
    /// keeps the launcher's PATH (`launcher_fallback`); `OrbitRuntime` resolves the owner user's
    /// login shell under `[workflow.validation_env]`, including workspace-selected
    /// build output bindings applied by host validation runners.
    fn validation_subprocess_environment(&self) -> orbit_exec::ValidationEnvironment;

    /// Rebind durable runtime handles after an external CLI provider exits.
    ///
    /// Provider sandboxes may receive explicit access to a SQLite database and
    /// its WAL sidecars. A host with cached connections refreshes them here so
    /// completion audit and checkpoints target the authoritative files.
    fn refresh_persistence_after_cli_provider(&self) -> Result<(), OrbitError>;

    // ── Runtime services ───────────────────────────────────────────────

    fn record_event(&self, event: OrbitEvent) -> Result<(), OrbitError>;
    fn list_job_runs_for_gc(&self) -> Result<Vec<JobRun>, OrbitError>;

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

    /// [ORB-25967] Record a provider's complete usage reading, taken at one
    /// instant for one provider account. Each observation is stored as
    /// `record_provider_limit` stores it, and the account's `event`
    /// observations of that provider an older reading stored and this one
    /// no longer reports are retired.
    fn record_provider_usage_reading(
        &self,
        observations: &[ProviderLimitObservation],
    ) -> Result<(), OrbitError>;
    fn activity_implementer_identity(
        &self,
        input: &Value,
    ) -> Result<(Option<String>, Option<String>), OrbitError>;

    /// Return the exact model string persisted when the run's crew was
    /// resolved. Workflow commit attribution treats this as opaque config
    /// data and falls back when the run or model is unavailable.
    fn resolved_crew_model(&self, run_id: &str) -> Result<Option<String>, OrbitError>;
    fn run_tool_with_context(
        &self,
        name: &str,
        input: Value,
        tool_context: ToolContext,
    ) -> Result<Value, OrbitError>;

    /// Execute an engine-private VCS/PR operation for deterministic shipment.
    ///
    /// Unlike `run_tool_with_context`, this boundary never consults
    /// the public Tool registry, public authorization, or an activity
    /// allowlist. `OrbitRuntime` answers with
    /// [`crate::run_private_vcs_operation`]; tests may substitute an
    /// in-memory fake.
    fn run_private_vcs_operation(&self, operation: &str, input: Value)
    -> Result<Value, OrbitError>;
    fn resolved_agent_model_pair(&self, agent_cli: &str) -> Option<AgentModelPair>;
}
