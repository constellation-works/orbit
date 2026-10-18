//! The unified capability boundary between the engine and its runtime.

use orbit_common::OrbitError;
use orbit_store::contracts::JobRunStepParams;
use orbit_store::contracts::{InvocationQuery, InvocationRecord, KeptClaimCandidate};
use orbit_tools::{FsAuditLogger, ToolContext};
use orbit_types::identity::AgentModelPair;
use orbit_types::policy::Role;
use orbit_types::record::OrbitEvent;
use orbit_types::task::{
    ContextWideningStep, ExternalRef, Task, TaskArtifact, TaskComment, TaskHistoryEntry,
    TaskPriority, TaskStatus,
};
use orbit_types::telemetry::{InvocationTrace, ProviderLimitObservation};
use orbit_types::workflow::{
    DeferralProbe, JobRun, JobRunStartOutcome, JobRunState, NamespaceProbe, PipelineState,
};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;

use crate::activity_job::{
    DispatchError, ResolvedCliExecutor, ResolvedSandbox, ResolvedShellExecutor,
};

use super::{
    ClaimExecutionContext, CrewConfig, FinalRecoveryAdmission, FinalRecoveryAdmissionRequest,
    FinalRecoveryApplication, FinalRecoveryApplied, HandoffLandingContext, HandoffLandingUpdate,
    PluginBrokerHandle, PluginBrokerRun, PrAutoFixAdmission, PrAutoFixRequest, PrConfig,
    RebaseRecoveryAttemptScope, ResolvedActivityTools, ReviewLandingRequest, ReviewReleaseRequest,
    ReviewReportCorrectionRequest, ReviewerInvocationRequest, ScratchGcReport,
    StepRecoveryAdmission, StepRecoveryDecisionRead, StepRecoveryDecisionRequest,
    StepRecoveryDecisionSlot, TaskActivityUpdate, TaskAutomationUpdate, WorktreeGcTaskLookup,
};

/// The single capability boundary between the job executor and its runtime.
///
/// Deterministic actions, task/run persistence, environment resolution, agent
/// dispatch, and audit/checkpoint hooks all cross this boundary exactly once.
///
/// Every method is required, so a new capability fails to compile until the
/// production host answers it. Test doubles implement
/// `test_support::NullRuntimeHost` instead, which answers each capability the
/// way a host without it does.
pub trait RuntimeHost: Send + Sync {
    /// Start this run's plugin broker before a sandboxed provider is spawned.
    ///
    /// `Ok(None)` means the host offers no broker. An error names why a
    /// broker-capable host could not bind one; the step still runs without
    /// it.
    fn start_plugin_broker(
        &self,
        run: &PluginBrokerRun,
    ) -> Result<Option<Box<dyn PluginBrokerHandle>>, OrbitError>;

    fn register_worker_pid_namespace(&self, pid: u32) -> Result<(), OrbitError>;

    fn register_worker_process(&self, pid: u32) -> Result<(), OrbitError>;

    fn worker_invocation(&self) -> Option<orbit_types::tool::WorkerInvocation>;
    /// Record a direct landing's intent before it lands. A host without a
    /// scheduler store observes nothing.
    fn record_direct_landing_intent(
        &self,
        request: &orbit_types::workflow::automation::DirectLandingRequest,
    ) -> Result<(), OrbitError>;
    /// Coverage evidence `task_id` submitted as a delivery automation action,
    /// when the automation's own settlement accepts or would accept it for the
    /// frozen batch [ORB-14837]. A host without delivery automation has none.
    fn accepted_automation_coverage(
        &self,
        task_id: &str,
    ) -> Result<Option<orbit_types::workflow::automation::CoverageEvidence>, OrbitError>;

    fn insert_job_run(
        &self,
        job_id: &str,
        attempt: u32,
        scheduled_at: chrono::DateTime<chrono::Utc>,
        input: Option<serde_json::Value>,
        retry_source_run_id: Option<String>,
    ) -> Result<JobRun, OrbitError>;
    fn mark_job_run_running(
        &self,
        run_id: &str,
        started_at: chrono::DateTime<chrono::Utc>,
        pid: u32,
    ) -> Result<JobRunStartOutcome, OrbitError>;
    fn complete_job_run_step(
        &self,
        run_id: &str,
        params: &JobRunStepParams,
    ) -> Result<bool, OrbitError>;
    fn finalize_job_run(
        &self,
        run_id: &str,
        state: JobRunState,
        finished_at: chrono::DateTime<chrono::Utc>,
        duration_ms: Option<u64>,
    ) -> Result<bool, OrbitError>;
    fn get_job_run(&self, run_id: &str) -> Result<Option<JobRun>, OrbitError>;
    fn read_run_state(&self, run_id: &str) -> Result<Option<PipelineState>, OrbitError>;
    fn write_run_state(&self, run_id: &str, state: &PipelineState) -> Result<(), OrbitError>;

    fn get_task(&self, task_id: &str) -> Result<Task, OrbitError>;
    /// Retrieve only one named, complete inline evidence payload. Implementations
    /// enforce the existing artifact read bound and owning task/path authority.
    fn get_task_artifact(
        &self,
        task_id: &str,
        path: &str,
    ) -> Result<Option<TaskArtifact>, OrbitError>;
    fn get_task_artifacts(&self, task_id: &str) -> Result<Vec<TaskArtifact>, OrbitError>;
    fn get_task_comments(&self, task_id: &str) -> Result<Vec<TaskComment>, OrbitError>;
    fn get_task_history(&self, task_id: &str) -> Result<Vec<TaskHistoryEntry>, OrbitError>;
    fn list_tasks_filtered(
        &self,
        status: Option<TaskStatus>,
        priority: Option<TaskPriority>,
        parent_id: Option<&str>,
        job_run_id: Option<&str>,
        external_ref: Option<&ExternalRef>,
        has_external_ref_system: Option<&str>,
    ) -> Result<Vec<Task>, OrbitError>;
    /// The tasks run `run_id` bound on the machine that executes it.
    ///
    /// A run id is unique only within one machine's store, so two machines'
    /// runs can share one [ORB-13649]. A runtime that records where each
    /// binding executed scopes this lookup to the executing machine; one
    /// that does not reads by run id alone.
    fn list_run_tasks(&self, run_id: &str) -> Result<Vec<Task>, OrbitError>;

    fn start_task(
        &self,
        task_id: &str,
        note: Option<String>,
        comment: Option<String>,
    ) -> Result<Task, OrbitError>;
    fn admit_task_for_workflow(&self, task_id: &str, workflow: &str) -> Result<Task, OrbitError>;
    fn update_task_from_activity(
        &self,
        task_id: &str,
        update: TaskActivityUpdate,
    ) -> Result<Task, OrbitError>;
    fn apply_task_automation_update(
        &self,
        task_id: &str,
        update: TaskAutomationUpdate,
    ) -> Result<(), OrbitError>;

    /// Append an exact `file:` selector to `task_id` for each of `paths` its
    /// selectors do not cover, recording `step` and `activity` as the
    /// provenance in task history, and return the selectors appended.
    ///
    /// Agents may change any path the work requires; delivery widens the
    /// task's declaration rather than refusing the change. A claimed leaf
    /// widens nothing here: the owner widens at handoff acceptance. Hosts
    /// without task records widen nothing.
    fn widen_task_context_files(
        &self,
        task_id: &str,
        run_id: &str,
        step: ContextWideningStep,
        activity: &str,
        paths: &[String],
    ) -> Result<Vec<String>, OrbitError>;

    // ── Step-recovery hooks [ORB-11332] ────────────────────────────────

    /// Ask before dispatching a step-recovery hook. A host that gates
    /// recovery answers `Denied` with the reason.
    fn authorize_step_recovery(
        &self,
        run_id: &str,
        step_id: &str,
    ) -> Result<StepRecoveryAdmission, OrbitError>;
    /// Allocate the run-local file one `step_failure_recovery` invocation
    /// writes its decision to, beneath the assigned worktree's scratch
    /// directory [ORB-14152]. Hosts without the capability return `Ok(None)`:
    /// the invocation runs without a slot and keeps the legacy admission, one
    /// post-recovery attempt whenever recovery completes. An error refuses
    /// the recovery before its provider launches.
    fn allocate_step_recovery_decision(
        &self,
        request: &StepRecoveryDecisionRequest,
    ) -> Result<Option<StepRecoveryDecisionSlot>, OrbitError>;
    /// Read back and verify the decision a completed invocation left in
    /// `slot`. An error means the slot could not be read and authorizes no
    /// post-recovery attempt.
    fn read_step_recovery_decision(
        &self,
        slot: &StepRecoveryDecisionSlot,
    ) -> Result<StepRecoveryDecisionRead, OrbitError>;
    /// Revalidate the live owner immediately before a recovery hook asks the
    /// host process to mutate Git metadata. Agent subprocesses cannot confer
    /// this authority through their response payload.
    fn validate_step_recovery_mutation(
        &self,
        run_id: &str,
        step_id: &str,
        task_ids: &[String],
        workspace_path: &Path,
    ) -> Result<(), OrbitError>;
    /// The immutable source-repair admission on this run, if any. Hosts that
    /// cannot admit this recovery return none without requiring run storage.
    fn source_repair_admission(
        &self,
        run_id: &str,
    ) -> Result<Option<orbit_types::workflow::SourceRepairAdmission>, OrbitError>;
    /// Authenticate an owner-local publication preserved by final recovery.
    fn owner_published_candidate(
        &self,
        task_id: &str,
        source_run_id: Option<&str>,
    ) -> Result<Option<orbit_types::workflow::SourceRepairBinding>, OrbitError>;
    /// Read publication authority from this run's successful host adoption.
    fn owner_publication_binding(
        &self,
        run_id: &str,
    ) -> Result<Option<orbit_types::workflow::SourceRepairBinding>, OrbitError>;
    /// Recheck the durable operator admission and exact source/scope before
    /// applying a cancelled owner's published candidate. Other hosts refuse.
    fn validate_source_repair(&self, run_id: &str) -> Result<(), OrbitError>;
    /// Admit one `--auto-fix` repair of a pull request's failed required
    /// checks or merge conflict before any agent runs. The host draws the
    /// run's repair crew from `workflow.purpose_crews.auto_fix` (the low
    /// complexity pool while that purpose is unset) and decides the
    /// request against the pull request's durable ledger under the primary
    /// task's lock, recording an admitted attempt in the same critical
    /// section.
    fn admit_pr_auto_fix(
        &self,
        request: &PrAutoFixRequest,
    ) -> Result<PrAutoFixAdmission, OrbitError>;

    /// Reserve the cumulative infrastructure rerun before mutation, under the
    /// owner task lock. Hosts without a durable ledger refuse by default.
    fn admit_pr_infrastructure(
        &self,
        _execution: &str,
        _task_ids: &[String],
        _evidence: &orbit_types::workflow::PrInfrastructureEvidence,
    ) -> Result<orbit_types::workflow::PrInfrastructureAdmission, OrbitError> {
        Ok(orbit_types::workflow::PrInfrastructureAdmission::Refused(
            "this host cannot durably reserve infrastructure recovery".into(),
        ))
    }

    /// Recheck current accepted handoff authority/native evidence and reserve
    /// its base update under the same owner lock. Generic hosts refuse.
    fn admit_handoff_base_update(
        &self,
        _handoff_id: &str,
        _request: &orbit_types::workflow::PrBaseUpdateReservation,
    ) -> Result<orbit_types::workflow::PrBaseUpdateAdmission, OrbitError> {
        Ok(orbit_types::workflow::PrBaseUpdateAdmission::Refused(
            "current handoff authority is unavailable".into(),
        ))
    }

    /// Reserve the one cumulative provider base update under the owning task
    /// lock. A host without durable owner admission cannot mutate.
    fn admit_pr_base_update(
        &self,
        _task_ids: &[String],
        _request: &orbit_types::workflow::PrBaseUpdateReservation,
    ) -> Result<orbit_types::workflow::PrBaseUpdateAdmission, OrbitError> {
        Ok(orbit_types::workflow::PrBaseUpdateAdmission::Refused(
            "this host cannot durably reserve a base update".into(),
        ))
    }

    // ── Before-PR review coverage [ORB-11333] ───────────────────────────

    /// Record how a reviewed candidate actually landed after a managed
    /// merge. Hosts without review evidence keep the pre-existing behavior.
    fn record_review_landing(&self, request: &ReviewLandingRequest) -> Result<(), OrbitError>;

    /// Close a review attempt that ended without a verdict, charging the
    /// reviewer runtime spent, so a failed or terminated reviewer step never
    /// leaves its attempt open. Hosts without review evidence have nothing
    /// to close.
    fn release_review_attempt(&self, request: &ReviewReleaseRequest) -> Result<(), OrbitError>;

    /// Record a reviewer invocation starting or ending for its attempt.
    /// For a start, returns the seconds the invocation may run, which become
    /// the reviewer process's wall clock whatever the activity declares
    /// [ORB-13992]. Hosts without review evidence have nothing to charge or
    /// bound, and the activity's own wall clock applies.
    fn record_reviewer_invocation(
        &self,
        request: &ReviewerInvocationRequest,
    ) -> Result<Option<u64>, OrbitError>;

    /// The defect settlement would find in the report of a reviewer that
    /// just returned, when the reviewer can correct it without rerunning a
    /// check [ORB-14616]. The engine then asks that reviewer, once, to
    /// correct the report before settlement. Hosts without review evidence
    /// have nothing to judge.
    fn review_report_correction(
        &self,
        request: &ReviewReportCorrectionRequest,
    ) -> Result<Option<String>, OrbitError>;

    // ── Owner landing consumer [ORB-12499] ─────────────────────────────

    /// Read the owner's authorized handoff for a landing attempt. Hosts without
    /// an owner coordination store have no landing work.
    fn handoff_landing_context(
        &self,
        handoff_id: &str,
    ) -> Result<HandoffLandingContext, OrbitError>;

    /// Record a landing step durably. The host rechecks current authority, the
    /// exact candidate and the pinned validation evidence inside its own
    /// transaction; an error refuses the step.
    fn record_handoff_landing(&self, update: &HandoffLandingUpdate) -> Result<(), OrbitError>;

    // ── Claimed distributed leaf execution [ORB-12616] ─────────────────

    /// The machine this runtime executes on, as the
    /// [`orbit_types::task::ExecutionLocation`] of its own runs names it. Run
    /// ids are unique only per machine [ORB-13649], so a task bound to a run
    /// on any other machine — or on any machine, when this host has no
    /// identity — names a run this store does not hold.
    fn local_machine_id(&self) -> Option<String>;

    /// [ORB-14603] The candidate the task's latest claim settlement kept, as
    /// offered to this machine's own run of the task, or why that run
    /// implements afresh. Hosts that keep no claims kept none.
    fn kept_claim_candidate(&self, task_id: &str)
    -> Result<Option<KeptClaimCandidate>, OrbitError>;

    /// The trusted claim this process is executing under. Hosts with no worker
    /// binding have no claimed execution and refuse: a claimed leaf activity
    /// must never fall back to an unauthenticated local identity.
    fn claim_execution_context(&self) -> Result<ClaimExecutionContext, OrbitError>;

    /// Attach one captured validation log to the claimed task on the owner, so
    /// the evidence the owner later re-reads lives in the owner's coordination
    /// store rather than on the executor's disk.
    fn attach_claim_validation_log(&self, path: &str, content: Vec<u8>) -> Result<(), OrbitError>;

    /// Record the typed handoff as this claim's durable pending settlement.
    /// It commits locally before any owner call, so a disconnect leaves
    /// exactly one immutable settlement for an idempotent retry.
    fn record_claim_handoff(
        &self,
        handoff: &orbit_types::workflow::handoff::TaskHandoff,
    ) -> Result<(), OrbitError>;

    /// Attach one captured required-validation log to a task the calling run
    /// owns, as owner-side evidence of the candidate it validated
    /// [ORB-13915]. The host refuses a task this run does not own.
    fn attach_task_validation_log(
        &self,
        task_id: &str,
        run_id: &str,
        path: &str,
        content: Vec<u8>,
    ) -> Result<(), OrbitError>;

    /// Record a passing isolated candidate replay in task history and file
    /// each flaky signature once in the owner's workspace.
    fn record_flaky_validation(
        &self,
        task_id: &str,
        record: &orbit_types::workflow::FlakyValidation,
    ) -> Result<(), OrbitError>;

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

    /// The OS this host runs implementers on, which decides the deferral
    /// kinds it admits [ORB-15587]: this process's OS for a local host.
    fn host_os(&self) -> Option<orbit_types::workflow::EvidenceHostOs>;

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
    fn run_private_vcs_operation(&self, operation: &str, input: Value)
    -> Result<Value, OrbitError>;
    fn resolved_agent_model_pair(&self, agent_cli: &str) -> Option<AgentModelPair>;
    /// Return the current agent model identity when this runtime is operating
    /// as an agent, or `None` when there is no model-bearing actor.
    fn actor_model_identity(&self) -> Option<String>;
    fn pr_config(&self) -> PrConfig;

    /// Dispatch a deterministic action by name. The host looks up `action`
    /// in its registry and returns the action's structured output.
    fn run_deterministic(
        &self,
        action: &str,
        config: &Value,
        input: &Value,
        tool_context: ToolContext,
    ) -> Result<Value, DispatchError>;

    /// Report whether `action` names a deterministic action this host's
    /// registry can actually dispatch.
    ///
    /// [ORB-10385] Catalog assets and the installed binary are separate
    /// artifacts: a workspace can load an activity whose `action:` the running
    /// runtime does not implement. Job validation consults this before the
    /// first step runs, so that skew fails admission instead of being
    /// discovered by a terminal failure hook after a task was admitted and
    /// implemented. A host that cannot enumerate its registry answers
    /// `true` and keeps the pre-ORB-10385 behavior of surfacing the miss at
    /// dispatch as
    /// [`DispatchError::DeterministicActionNotRegistered`]. Reporting `true`
    /// for an unknown action is therefore safe; reporting `false` for a
    /// dispatchable one would reject a healthy job.
    fn has_deterministic_action(&self, action: &str) -> bool;

    /// Resolve the CLI executor command and static args for a given v2
    /// provider name. Workspace / env overrides live
    /// in the host so the engine's CLI runner stays environment-agnostic.
    /// Returning an error is the structured failure path when a provider has no
    /// CLI mapping (e.g. `openai_compat` which is HTTP-only).
    fn resolve_cli_executor(&self, provider: &str) -> Result<ResolvedCliExecutor, DispatchError>;

    /// Resolve the registered `local_shell` executor definition backing a
    /// deterministic shell step [ORB-11294].
    ///
    /// Separate from [`RuntimeHost::resolve_cli_executor`] on purpose: that
    /// boundary resolves an *agent* provider and rejects anything that is not
    /// `direct_agent` / `agent_cli`. A shell step carries no model, prompt, or
    /// agent tool authority, so it resolves its own executor family and never
    /// borrows an agent's.
    ///
    /// A host without an executor store answers the empty definition: the activity's own `config` block
    /// then has to name the command outright.
    fn resolve_local_shell_executor(
        &self,
        executor: &str,
    ) -> Result<ResolvedShellExecutor, DispatchError>;

    /// Return provider-specific CLI runtime config for agent execution.
    ///
    /// Most providers ignore this today. Codex uses it for sandbox,
    /// approval-policy, and writable-directory arguments that must stay dynamic
    /// rather than living in the static executor definition.
    fn provider_cli_config(&self, provider: &str) -> HashMap<String, String>;

    /// Resolve the OS sandbox payload for a CLI invocation. The host reads
    /// the executor's `sandbox` declaration, materializes the activity's
    /// `fs_profile` against the active policy, and compiles the result via
    /// `orbit-exec`. Returns `Ok(None)` when the executor has no sandbox
    /// declared (today's behavior). Returns a structured error on
    /// platform mismatch (e.g. `macos-sandbox-exec` on Linux) so the
    /// activity fails closed at dispatch time.
    ///
    /// `subprocess_cwd` is the resolved working directory the subprocess
    /// will run in. The host uses it to re-allow the active worktree path
    /// after the policy's `denyModify .orbit/**` rule when the cwd is a
    /// jrun worktree under `.orbit/state/worktrees/`. Without this, every
    /// non-codex provider (claude/gemini) cannot write inside its own
    /// worktree because the deny rule wins last-match. See T20260508-17.
    fn resolve_executor_sandbox(
        &self,
        provider: &str,
        fs_profile: Option<&str>,
        subprocess_cwd: Option<&Path>,
    ) -> Result<Option<ResolvedSandbox>, DispatchError>;

    /// Run `probe` in the implementer's lane. Hosts must opt into this
    /// capability and apply the sandbox resolved for the provider, profile
    /// and checkout. A host without the capability reports an unsupported,
    /// unexecuted probe, which cannot justify a deferral [ORB-15516].
    fn run_deferral_probe(
        &self,
        probe: &DeferralProbe,
        provider: Option<&str>,
        fs_profile: Option<&str>,
        cwd: &Path,
    ) -> NamespaceProbe;

    /// Optional task snapshot to embed in the CLI agent envelope `step_id` of
    /// `run_id` dispatches.
    ///
    /// The engine keeps this as untyped JSON so orbit-core can source task data
    /// without leaking store or task-query details into orbit-engine. A host
    /// with run storage and `workflow.mid_run_steering.enabled` records, while
    /// building the snapshot, the newest task comment it carries as the run's
    /// steering watermark for that step. At a later prompt of the same run, it
    /// delivers operator comments past that watermark as a leading `steering`
    /// block and records the delivery; otherwise the snapshot is the same
    /// either way.
    fn task_context_for_agent_input(
        &self,
        run_id: &str,
        step_id: &str,
        input: &Value,
    ) -> Result<Option<Value>, DispatchError>;

    /// Resolve a deny-mode activity's callable tools for one agent launch.
    ///
    /// `effective_tools` is every registered agent-facing tool the disallow
    /// list does not cover. A requirement never overrides that list.
    /// `agent_implement`, and any activity that is not a shipped
    /// non-implementer, admits `required_tools` fail-closed: unknown,
    /// inactive, malformed and denied requirements refuse dispatch, naming
    /// the tool and `activity`. Final recovery, step recovery, reviewers and
    /// the other shipped non-implementers drop a covered requirement from
    /// `requested_tools` and record why on `omitted_requirement_notes`.
    /// A host without a tool registry cannot compute the set, so it refuses
    /// rather than guessing.
    fn resolve_activity_tool_denials(
        &self,
        task_ids: &[String],
        activity: &str,
        disallow_list: &[String],
    ) -> Result<ResolvedActivityTools, DispatchError>;

    /// Compose and validate the exact task-scoped tools for one agent launch.
    /// Hosts without a task/tool registry preserve the activity baseline.
    fn resolve_activity_tools(
        &self,
        task_ids: &[String],
        baseline_tools: &[String],
    ) -> Result<ResolvedActivityTools, DispatchError>;

    /// Persist a durable checkpoint after a completed top-level job step
    /// (ORB-10002). `output` is the completing step's own raw output;
    /// `compound_outputs` holds the other pipeline entries a compound step
    /// (`parallel:`, `fan_out:`, `loop:`) exposed — nested step outputs and
    /// nested fan-in aliases — and is empty for a target step. The host
    /// accumulates by step, so the bytes handed over per checkpoint never
    /// grow with the run.
    ///
    /// Hosts with run persistence (orbit-core) record this into the run's
    /// `PipelineState` (`step_outputs[step_index]`,
    /// `compound_outputs[step_index]`, and `pipeline` by key) so an
    /// interrupted run can be resumed without re-executing completed steps.
    /// Both halves belong to one write: a step recorded as completed without
    /// its compound outputs would resume with those entries missing. A host
    /// without run storage records nothing. Checkpoint failures are non-fatal to the run: the executor
    /// logs and continues.
    fn checkpoint_step(
        &self,
        run_id: &str,
        step_index: u32,
        step_id: &str,
        output: &Value,
        compound_outputs: &BTreeMap<String, Value>,
    ) -> Result<(), DispatchError>;

    /// Persist diagnostic output from an incomplete step without marking it
    /// successful or making it eligible for resume skipping.
    fn checkpoint_step_diagnostic(
        &self,
        _run_id: &str,
        _step_id: &str,
        _output: &Value,
    ) -> Result<(), DispatchError> {
        Ok(())
    }

    /// Persist a successful terminal failure activity separately from normal
    /// step checkpoints. The failed step remains failed; this records only the
    /// recovery evidence a later resume may authenticate.
    fn checkpoint_failure_activity(
        &self,
        run_id: &str,
        activity_name: &str,
        failed_step_id: &str,
        output: &Value,
    ) -> Result<(), DispatchError>;

    /// Read only this run's bounded, redacted worker log tail for built-in
    /// step and final recovery. `None` means the run has no worker log; read failures must
    /// remain distinguishable from an empty log. This host capability is
    /// internal and grants no agent-facing run observation tool.
    fn final_recovery_log_tail(&self, run_id: &str) -> Result<Option<String>, OrbitError>;

    /// [ORB-13907] Admit a job's final recovery for a failed run, once.
    ///
    /// A host with run storage records the admission in the run's state
    /// before answering `Admitted`, so neither a crash during the activity
    /// nor any resume of the run invokes it again; it skips when final
    /// recovery is disabled (an empty `workflow.final_recovery_crews`), when
    /// the run already spent it, or when the run is no longer running. A host
    /// without run storage skips: it cannot promise "once".
    fn admit_final_recovery(
        &self,
        run_id: &str,
        request: &FinalRecoveryAdmissionRequest,
    ) -> Result<FinalRecoveryAdmission, OrbitError>;

    /// Keep one absolute completion budget across retries of a persisted run.
    /// An unpersisted host uses the current invocation's deadline.
    fn completion_deadline(
        &self,
        _run_id: &str,
        _pull_request: &str,
        proposed: u64,
    ) -> Result<u64, OrbitError> {
        Ok(proposed)
    }

    /// Bind native validation to task meaning and the effective completion policy.
    /// Hosts without this evidence never reuse a published completion prefix.
    fn completion_policy_digest(
        &self,
        _run_id: &str,
        _task_ids: &[String],
    ) -> Result<Option<String>, OrbitError> {
        Ok(None)
    }

    /// Authenticate an unchanged publication before the once-only final recovery
    /// retries a read-only completion observation in the same active execution.
    fn authenticate_completion_tail(
        &self,
        _run_id: &str,
        _task_id: &str,
        _step_id: &str,
    ) -> Result<bool, OrbitError> {
        Ok(false)
    }

    /// [ORB-13907] Act on an admitted final recovery's decision: record it,
    /// and apply every decision but `resume` to the task through the
    /// deterministic applier — or, for a claimed leaf, hand it to the claim
    /// settlement instead of writing the owner's task.
    fn apply_final_recovery(
        &self,
        run_id: &str,
        application: &FinalRecoveryApplication,
    ) -> Result<FinalRecoveryApplied, OrbitError>;

    /// Reserve the host-only identity of one preparation or rebase recovery
    /// of `step_id`, before issuing its certificate (and, for conflicts,
    /// before its provider runs).
    ///
    /// The host assigns the attempt; the caller only carries it, in memory, to
    /// the checkpoint it stamps as `recovery_attempt`. A later reservation for
    /// the same run and step supersedes this one, so only the newest admitted
    /// recovery can be certified. Hosts without a recovery authority refuse,
    /// which stops the recovery before the provider launches.
    fn begin_rebase_recovery_attempt(
        &self,
        run_id: &str,
        step_id: &str,
        scope: &RebaseRecoveryAttemptScope,
    ) -> Result<u64, DispatchError>;

    /// Persist an exact host-validated preparation or recovered rebase before
    /// reporting success. Preparations use separate step identities from
    /// rewrites and bind their entire observation payload. Unlike ordinary
    /// step checkpoints, durability failure is fatal.
    /// `output` carries the attempt reserved by
    /// [`Self::begin_rebase_recovery_attempt`] as `recovery_attempt`.
    fn checkpoint_rebase_recovery(
        &self,
        run_id: &str,
        step_id: &str,
        output: &Value,
    ) -> Result<(), DispatchError>;

    /// Whether `checkpoint` is exactly the current recovery evidence this host
    /// certified for `run_id` / `step_id`: its own attempt's evidence, with no
    /// later attempt of that step certified since.
    ///
    /// The run store a checkpoint is read back from is writable by managed
    /// leaves, so the stored bytes are progress data. Authority lives in a
    /// host-only record this method consults. Hosts without one certify
    /// nothing and therefore authenticate nothing.
    fn verify_rebase_recovery(
        &self,
        run_id: &str,
        step_id: &str,
        checkpoint: &Value,
    ) -> Result<bool, OrbitError>;

    /// Every recovery attempt this host reserved for `run_id` / `step_id`,
    /// oldest first, read from the same host-only record that assigns them.
    /// Completion counts the distinct bases a step rebased onto to bound how
    /// often it follows a moving base [ORB-14393].
    fn rebase_recovery_attempts(
        &self,
        run_id: &str,
        step_id: &str,
    ) -> Result<Vec<RebaseRecoveryAttemptScope>, OrbitError>;

    fn tool_context_for_activity(
        &self,
        run_id: Option<&str>,
        fs_profile: Option<&str>,
        fs_audit: Option<Arc<dyn FsAuditLogger>>,
        proc_allowed_programs: Option<&[String]>,
    ) -> ToolContext;

    fn persist_invocation_trace(
        &self,
        job_run_id: &str,
        activity_id: &str,
        provider: &str,
        model: Option<&str>,
        input: &Value,
        trace: &InvocationTrace,
    ) -> Result<(), DispatchError>;

    /// Resolve the crew selected for an activity dispatch. The engine passes
    /// rendered activity input when it contains an explicit `crew`; otherwise
    /// it passes the run input so the run's resolved crew is the fallback.
    /// Hosts without a crew registry may return `None` only when no explicit
    /// crew was requested, preserving inline settings in isolated tests.
    fn agent_crew_config_for_input(
        &self,
        input: &Value,
    ) -> Result<Option<CrewConfig>, DispatchError>;
}
