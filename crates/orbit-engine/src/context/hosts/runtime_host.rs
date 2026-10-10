//! The unified capability boundary between the engine and its runtime.

use orbit_common::OrbitError;
use orbit_common::security::child_env::allowlisted_child_env;
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
use orbit_types::workflow::{JobRun, JobRunStartOutcome, JobRunState, PipelineState};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;

use crate::activity_job::{
    DispatchError, ResolvedCliExecutor, ResolvedSandbox, ResolvedShellExecutor,
};

use super::task_update::{unsupported_dispatch_capability, unsupported_runtime_capability};
use super::{
    ClaimExecutionContext, CrewConfig, FinalRecoveryAdmission, FinalRecoveryAdmissionRequest,
    FinalRecoveryApplication, FinalRecoveryApplied, HandoffLandingContext, HandoffLandingUpdate,
    PluginBrokerHandle, PluginBrokerRun, PrConfig, RebaseRecoveryAttemptScope,
    ResolvedActivityTools, ReviewLandingRequest, ReviewReleaseRequest,
    ReviewReportCorrectionRequest, ReviewerInvocationRequest, ScratchGcReport,
    StepRecoveryAdmission, StepRecoveryDecisionRead, StepRecoveryDecisionRequest,
    StepRecoveryDecisionSlot, TaskActivityUpdate, TaskAutomationUpdate, WorktreeGcTaskLookup,
};

/// The single capability boundary between the job executor and its runtime.
///
/// Deterministic actions, task/run persistence, environment resolution, agent
/// dispatch, and audit/checkpoint hooks all cross this boundary exactly once.
pub trait RuntimeHost: Send + Sync {
    /// Start this run's plugin broker before a sandboxed provider is spawned.
    ///
    /// `Ok(None)` means the host offers no broker. An error names why a
    /// broker-capable host could not bind one; the step still runs without
    /// it.
    fn start_plugin_broker(
        &self,
        _run: &PluginBrokerRun,
    ) -> Result<Option<Box<dyn PluginBrokerHandle>>, OrbitError> {
        Ok(None)
    }

    fn register_worker_pid_namespace(&self, _pid: u32) -> Result<(), OrbitError> {
        if self.worker_invocation().is_some() {
            return Err(OrbitError::Execution(
                "worker namespace authority unavailable".into(),
            ));
        }
        Ok(())
    }

    fn register_worker_process(&self, _pid: u32) -> Result<(), OrbitError> {
        if self.worker_invocation().is_some() {
            return Err(OrbitError::Execution(
                "worker process authority unavailable".into(),
            ));
        }
        Ok(())
    }

    fn worker_invocation(&self) -> Option<orbit_types::tool::WorkerInvocation> {
        None
    }
    /// Optional observation hook; execution-only test hosts need no scheduler store.
    fn record_direct_landing_intent(
        &self,
        _request: &orbit_types::workflow::automation::DirectLandingRequest,
    ) -> Result<(), OrbitError> {
        Ok(())
    }
    /// Coverage evidence `task_id` submitted as a delivery automation action,
    /// when the automation's own settlement accepts or would accept it for the
    /// frozen batch [ORB-14837]. A host without delivery automation has none.
    fn accepted_automation_coverage(
        &self,
        _task_id: &str,
    ) -> Result<Option<orbit_types::workflow::automation::CoverageEvidence>, OrbitError> {
        Ok(None)
    }

    fn insert_job_run(
        &self,
        job_id: &str,
        attempt: u32,
        scheduled_at: chrono::DateTime<chrono::Utc>,
        input: Option<serde_json::Value>,
        retry_source_run_id: Option<String>,
    ) -> Result<JobRun, OrbitError> {
        let _ = (job_id, attempt, scheduled_at, input, retry_source_run_id);
        Err(unsupported_runtime_capability("insert_job_run"))
    }
    fn mark_job_run_running(
        &self,
        run_id: &str,
        started_at: chrono::DateTime<chrono::Utc>,
        pid: u32,
    ) -> Result<JobRunStartOutcome, OrbitError> {
        let _ = (run_id, started_at, pid);
        Err(unsupported_runtime_capability("mark_job_run_running"))
    }
    fn complete_job_run_step(
        &self,
        run_id: &str,
        params: &JobRunStepParams,
    ) -> Result<bool, OrbitError> {
        let _ = (run_id, params);
        Err(unsupported_runtime_capability("complete_job_run_step"))
    }
    fn finalize_job_run(
        &self,
        run_id: &str,
        state: JobRunState,
        finished_at: chrono::DateTime<chrono::Utc>,
        duration_ms: Option<u64>,
    ) -> Result<bool, OrbitError> {
        let _ = (run_id, state, finished_at, duration_ms);
        Err(unsupported_runtime_capability("finalize_job_run"))
    }
    fn get_job_run(&self, _run_id: &str) -> Result<Option<JobRun>, OrbitError> {
        Err(unsupported_runtime_capability("get_job_run"))
    }
    fn read_run_state(&self, _run_id: &str) -> Result<Option<PipelineState>, OrbitError> {
        Ok(None)
    }
    fn write_run_state(&self, _run_id: &str, _state: &PipelineState) -> Result<(), OrbitError> {
        Err(unsupported_runtime_capability("write_run_state"))
    }

    fn get_task(&self, _task_id: &str) -> Result<Task, OrbitError> {
        Err(unsupported_runtime_capability("get_task"))
    }
    fn get_task_artifacts(&self, _task_id: &str) -> Result<Vec<TaskArtifact>, OrbitError> {
        Ok(Vec::new())
    }
    fn get_task_comments(&self, _task_id: &str) -> Result<Vec<TaskComment>, OrbitError> {
        Ok(Vec::new())
    }
    fn get_task_history(&self, _task_id: &str) -> Result<Vec<TaskHistoryEntry>, OrbitError> {
        Ok(Vec::new())
    }
    fn list_tasks_filtered(
        &self,
        status: Option<TaskStatus>,
        priority: Option<TaskPriority>,
        parent_id: Option<&str>,
        job_run_id: Option<&str>,
        external_ref: Option<&ExternalRef>,
        has_external_ref_system: Option<&str>,
    ) -> Result<Vec<Task>, OrbitError> {
        let _ = (
            status,
            priority,
            parent_id,
            job_run_id,
            external_ref,
            has_external_ref_system,
        );
        Err(unsupported_runtime_capability("list_tasks_filtered"))
    }
    /// The tasks run `run_id` bound on the machine that executes it.
    ///
    /// A run id is unique only within one machine's store, so two machines'
    /// runs can share one [ORB-13649]. A runtime that records where each
    /// binding executed scopes this lookup to the executing machine; the
    /// default reads by run id alone.
    fn list_run_tasks(&self, run_id: &str) -> Result<Vec<Task>, OrbitError> {
        self.list_tasks_filtered(None, None, None, Some(run_id), None, None)
    }

    fn start_task(
        &self,
        task_id: &str,
        note: Option<String>,
        comment: Option<String>,
    ) -> Result<Task, OrbitError> {
        let _ = (task_id, note, comment);
        Err(unsupported_runtime_capability("start_task"))
    }
    fn admit_task_for_workflow(&self, _task_id: &str, _workflow: &str) -> Result<Task, OrbitError> {
        Err(unsupported_runtime_capability("admit_task_for_workflow"))
    }
    fn update_task_from_activity(
        &self,
        task_id: &str,
        update: TaskActivityUpdate,
    ) -> Result<Task, OrbitError> {
        let _ = (task_id, update);
        Err(unsupported_runtime_capability("update_task_from_activity"))
    }
    fn apply_task_automation_update(
        &self,
        task_id: &str,
        update: TaskAutomationUpdate,
    ) -> Result<(), OrbitError> {
        let _ = (task_id, update);
        Err(unsupported_runtime_capability(
            "apply_task_automation_update",
        ))
    }

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
    ) -> Result<Vec<String>, OrbitError> {
        let _ = (task_id, run_id, step, activity, paths);
        Ok(Vec::new())
    }

    // ── Operation-mode rechecks [ORB-11332] ────────────────────────────

    /// Ask before dispatching a step-recovery hook. Hosts without operation
    /// mode allow every recovery, which is the pre-existing behavior.
    fn authorize_step_recovery(
        &self,
        _run_id: &str,
        _step_id: &str,
    ) -> Result<StepRecoveryAdmission, OrbitError> {
        Ok(StepRecoveryAdmission::Allowed)
    }
    /// Record the wall time a reserved recovery episode consumed.
    fn settle_step_recovery(
        &self,
        _run_id: &str,
        _step_id: &str,
        _elapsed_seconds: u64,
    ) -> Result<(), OrbitError> {
        Ok(())
    }
    /// Allocate the run-local file one `step_failure_recovery` invocation
    /// writes its decision to, beneath the assigned worktree's scratch
    /// directory [ORB-14152]. Hosts without the capability return `Ok(None)`:
    /// the invocation runs without a slot and keeps the legacy admission, one
    /// post-recovery attempt whenever recovery completes. An error refuses
    /// the recovery before its provider launches.
    fn allocate_step_recovery_decision(
        &self,
        _request: &StepRecoveryDecisionRequest,
    ) -> Result<Option<StepRecoveryDecisionSlot>, OrbitError> {
        Ok(None)
    }
    /// Read back and verify the decision a completed invocation left in
    /// `slot`. An error means the slot could not be read and authorizes no
    /// post-recovery attempt.
    fn read_step_recovery_decision(
        &self,
        _slot: &StepRecoveryDecisionSlot,
    ) -> Result<StepRecoveryDecisionRead, OrbitError> {
        Err(unsupported_runtime_capability(
            "read_step_recovery_decision",
        ))
    }
    /// Revalidate the live owner immediately before a recovery hook asks the
    /// host process to mutate Git metadata. Agent subprocesses cannot confer
    /// this authority through their response payload.
    fn validate_step_recovery_mutation(
        &self,
        _run_id: &str,
        _step_id: &str,
        _task_ids: &[String],
        _workspace_path: &Path,
    ) -> Result<(), OrbitError> {
        Err(unsupported_runtime_capability(
            "validate_step_recovery_mutation",
        ))
    }
    /// Recheck the run's authority immediately before the guarded
    /// `review -> done` transition. An error refuses completion.
    fn authorize_task_completion(
        &self,
        _run_id: &str,
        _task_ids: &[String],
    ) -> Result<(), OrbitError> {
        Ok(())
    }

    // ── Before-PR review coverage [ORB-11333] ───────────────────────────

    /// Record how a reviewed candidate actually landed after a managed
    /// merge. Hosts without review evidence keep the pre-existing behavior.
    fn record_review_landing(&self, _request: &ReviewLandingRequest) -> Result<(), OrbitError> {
        Ok(())
    }

    /// Close a review attempt that ended without a verdict, charging the
    /// reviewer runtime spent, so a failed or terminated reviewer step never
    /// leaves its attempt open. Hosts without review evidence have nothing
    /// to close.
    fn release_review_attempt(&self, _request: &ReviewReleaseRequest) -> Result<(), OrbitError> {
        Ok(())
    }

    /// Record a reviewer invocation starting or ending for its attempt.
    /// For a start, returns the seconds the invocation may run, which become
    /// the reviewer process's wall clock whatever the activity declares
    /// [ORB-13992]. Hosts without review evidence have nothing to charge or
    /// bound, and the activity's own wall clock applies.
    fn record_reviewer_invocation(
        &self,
        _request: &ReviewerInvocationRequest,
    ) -> Result<Option<u64>, OrbitError> {
        Ok(None)
    }

    /// The defect settlement would find in the report of a reviewer that
    /// just returned, when the reviewer can correct it without rerunning a
    /// check [ORB-14616]. The engine then asks that reviewer, once, to
    /// correct the report before settlement. Hosts without review evidence
    /// have nothing to judge.
    fn review_report_correction(
        &self,
        _request: &ReviewReportCorrectionRequest,
    ) -> Result<Option<String>, OrbitError> {
        Ok(None)
    }

    // ── Owner landing consumer [ORB-12499] ─────────────────────────────

    /// Read the owner's authorized handoff for a landing attempt. Hosts without
    /// an owner coordination store have no landing work.
    fn handoff_landing_context(
        &self,
        _handoff_id: &str,
    ) -> Result<HandoffLandingContext, OrbitError> {
        Err(unsupported_runtime_capability("handoff_landing_context"))
    }

    /// Record a landing step durably. The host rechecks current authority, the
    /// exact candidate and the pinned validation evidence inside its own
    /// transaction; an error refuses the step.
    fn record_handoff_landing(&self, _update: &HandoffLandingUpdate) -> Result<(), OrbitError> {
        Err(unsupported_runtime_capability("record_handoff_landing"))
    }

    // ── Claimed distributed leaf execution [ORB-12616] ─────────────────

    /// The machine this runtime executes on, as the
    /// [`orbit_types::task::ExecutionLocation`] of its own runs names it. Run
    /// ids are unique only per machine [ORB-13649], so a task bound to a run
    /// on any other machine — or on any machine, when this host has no
    /// identity — names a run this store does not hold.
    fn local_machine_id(&self) -> Option<String> {
        None
    }

    /// [ORB-14603] The candidate the task's latest claim settlement kept, as
    /// offered to this machine's own run of the task, or why that run
    /// implements afresh. Hosts that keep no claims kept none.
    fn kept_claim_candidate(
        &self,
        _task_id: &str,
    ) -> Result<Option<KeptClaimCandidate>, OrbitError> {
        Ok(None)
    }

    /// The trusted claim this process is executing under. Hosts with no worker
    /// binding have no claimed execution and refuse: a claimed leaf activity
    /// must never fall back to an unauthenticated local identity.
    fn claim_execution_context(&self) -> Result<ClaimExecutionContext, OrbitError> {
        Err(unsupported_runtime_capability("claim_execution_context"))
    }

    /// Attach one captured validation log to the claimed task on the owner, so
    /// the evidence the owner later re-reads lives in the owner's coordination
    /// store rather than on the executor's disk.
    fn attach_claim_validation_log(
        &self,
        _path: &str,
        _content: Vec<u8>,
    ) -> Result<(), OrbitError> {
        Err(unsupported_runtime_capability(
            "attach_claim_validation_log",
        ))
    }

    /// Record the typed handoff as this claim's durable pending settlement.
    /// It commits locally before any owner call, so a disconnect leaves
    /// exactly one immutable settlement for an idempotent retry.
    fn record_claim_handoff(
        &self,
        _handoff: &orbit_types::workflow::handoff::TaskHandoff,
    ) -> Result<(), OrbitError> {
        Err(unsupported_runtime_capability("record_claim_handoff"))
    }

    /// Attach one captured required-validation log to a task the calling run
    /// owns, as owner-side evidence of the candidate it validated
    /// [ORB-13915]. The host refuses a task this run does not own.
    fn attach_task_validation_log(
        &self,
        _task_id: &str,
        _run_id: &str,
        _path: &str,
        _content: Vec<u8>,
    ) -> Result<(), OrbitError> {
        Err(unsupported_runtime_capability("attach_task_validation_log"))
    }

    // ── Config accessors (implementors provide these) ──────────────────

    /// Commands every delivered candidate must pass
    /// (`workflow.required_validation_commands`). Empty means no required
    /// check: the owner delivery path validates nothing.
    fn required_validation_commands(&self) -> Vec<String> {
        Vec::new()
    }

    /// Returns provider-agnostic key-value configuration that is forwarded
    /// to the selected provider factory so it can decode any provider-specific
    /// settings (for example Codex reads `"sandbox"`, `"approval_policy"` and
    /// `"allow_login_shell"`).
    fn agent_provider_config(&self) -> HashMap<String, String> {
        HashMap::new()
    }
    /// The complete environment an agent subprocess is launched with.
    ///
    /// Launchers clear the child environment and apply exactly what this
    /// returns, so anything absent here does not reach the provider.
    /// `required_env_vars` are the names the provider runtime declares it
    /// needs. The default is the built-in baseline plus those extras: a host
    /// with no configuration still starts a provider, but never forwards
    /// ambient credentials. `OrbitRuntime` overrides it with the operator's
    /// `[execution.env]` policy, with `workflow.validation_env.path` ahead of
    /// PATH. [ORB-10917] [ORB-15204]
    fn agent_subprocess_environment(&self, required_env_vars: &[&str]) -> Vec<(String, String)> {
        allowlisted_child_env(&[], required_env_vars)
    }
    /// Whether a Claude activity must be launched with its own credential
    /// (`CLAUDE_CODE_OAUTH_TOKEN` or `ANTHROPIC_API_KEY`) rather than start on
    /// the login the Claude Desktop app shares [ORB-15154].
    ///
    /// The Desktop revokes that shared login when it refreshes, so an
    /// unattended run that starts on it fails with a 401 mid-way. The default
    /// is `false` so a host that owns no such login (tests, embedded hosts)
    /// is unaffected; `OrbitRuntime` answers `true` on macOS.
    fn requires_claude_worker_credential(&self) -> bool {
        false
    }
    /// The environment owner-side repository tooling runs in: required
    /// validation and `local_shell` steps [ORB-13987].
    ///
    /// It starts from the agent subprocess environment, so the allowlist still
    /// decides every variable, but PATH and toolchain locators must not depend
    /// on whatever launched the worker. The default keeps the launcher's PATH
    /// (`launcher_fallback`); `OrbitRuntime` resolves the owner user's login
    /// shell under `[workflow.validation_env]`.
    fn validation_subprocess_environment(&self) -> orbit_exec::ValidationEnvironment {
        orbit_exec::ValidationEnvironment::launcher(self.agent_subprocess_environment(&[]))
    }
    /// The authoritative shared Orbit registry root to hand a spawned CLI
    /// agent as `ORBIT_REGISTRY_ROOT`.
    ///
    /// This is the registry that owns the task store, not the dispatching
    /// checkout's workspace `.orbit`. The locator selects only the global
    /// registry. Workspace ownership for nested tool calls is the separate
    /// logical selector from [`RuntimeHost::orbit_workspace_selector`].
    /// [ORB-10980] [ORB-11066] [ORB-11117]
    fn orbit_registry_root(&self) -> Option<String> {
        None
    }
    /// The trusted logical `ws_*` selector to hand a spawned CLI agent as
    /// `ORBIT_WORKSPACE`.
    ///
    /// Nested MCP and CLI tool calls use this identity instead of inferring
    /// durable ownership from a linked-worktree cwd. Hosts that are not bound
    /// to a registered workspace return `None`, and the child then keeps its
    /// ordinary fail-closed resolution. [ORB-11117]
    fn orbit_workspace_selector(&self) -> Option<String> {
        None
    }
    /// Rebind durable runtime handles after an external CLI provider exits.
    ///
    /// Provider sandboxes may receive explicit access to a SQLite database and
    /// its WAL sidecars. A host with cached connections refreshes them here so
    /// completion audit and checkpoints target the authoritative files.
    fn refresh_persistence_after_cli_provider(&self) -> Result<(), OrbitError> {
        Ok(())
    }

    // ── Default implementations (use accessors above) ──────────────────

    fn record_event(&self, _event: OrbitEvent) -> Result<(), OrbitError> {
        Ok(())
    }
    fn repo_root(&self) -> Result<String, OrbitError> {
        Err(unsupported_runtime_capability("repo_root"))
    }
    fn list_job_runs_for_gc(&self) -> Result<Vec<JobRun>, OrbitError> {
        Err(OrbitError::Execution(
            "worktree GC is not implemented for this runtime host".to_string(),
        ))
    }
    /// Admitted rebuildable paths, independent of checkout-authored commands.
    fn worktree_reclaim_patterns(&self) -> Vec<String> {
        vec!["target".to_string()]
    }
    /// A task's settlement state for worktree GC, read from the store that
    /// owns the workspace's tasks. `run_id` is the run whose worktree is
    /// being classified, so a replica can ask through that run's own claim
    /// route. The default reads this host's own store; a replica host
    /// overrides it to ask its owner.
    fn lookup_task_for_worktree_gc(&self, _run_id: &str, task_id: &str) -> WorktreeGcTaskLookup {
        match self.get_task(task_id) {
            Ok(task) => WorktreeGcTaskLookup::Found {
                status: task.status,
                pr_status: task.pr_status,
            },
            Err(_) => WorktreeGcTaskLookup::Unresolved,
        }
    }
    /// Stable scope for memoizing task lookups during one GC sweep. Replica
    /// hosts return the claim's owner selector, because one checkout may hold
    /// claims routed to different owners. `None` disables memoization.
    fn worktree_gc_task_lookup_scope(&self, _run_id: &str) -> Option<String> {
        Some("local".to_string())
    }
    /// Whether `run_id` is a claimed leaf whose claim this follower has
    /// settled with its owner. The owner then holds the leaf's delivery, so
    /// a terminal leaf's worktree is this machine's to reclaim without asking
    /// about the task. `Some` names the settlement for the GC report; the
    /// default host pulls no work and holds no claims.
    fn settled_claim_for_worktree_gc(&self, _run_id: &str) -> Option<String> {
        None
    }
    /// Prune this checkout's `.orbit/tmp` of top-level entries untouched for
    /// `retention_hours`. `None` means the host keeps no scratch directory to
    /// sweep.
    fn gc_scratch(&self, _retention_hours: u64) -> Result<Option<ScratchGcReport>, OrbitError> {
        Ok(None)
    }
    fn data_root(&self) -> &Path {
        Path::new("")
    }
    fn cancel_job_run(&self, run_id: &str) -> Result<(), OrbitError> {
        Err(OrbitError::Execution(format!(
            "cancel_job_run is not implemented for run '{run_id}'"
        )))
    }
    fn invocation_records(
        &self,
        _query: InvocationQuery,
    ) -> Result<Vec<InvocationRecord>, OrbitError> {
        Ok(Vec::new())
    }
    /// [ORB-14695] Record a provider usage limit a run observed in this
    /// host's provider-limit store. The latest observation per provider,
    /// model scope and window stands; an older one never replaces it.
    fn record_provider_limit(
        &self,
        _observation: &ProviderLimitObservation,
    ) -> Result<(), OrbitError> {
        Err(unsupported_runtime_capability("record_provider_limit"))
    }
    fn activity_implementer_identity(
        &self,
        _input: &Value,
    ) -> Result<(Option<String>, Option<String>), OrbitError> {
        Ok((None, None))
    }
    /// Return the exact model string persisted when the run's crew was
    /// resolved. Workflow commit attribution treats this as opaque config
    /// data and falls back when the run or model is unavailable.
    fn resolved_crew_model(&self, _run_id: &str) -> Result<Option<String>, OrbitError> {
        Ok(None)
    }
    fn run_tool_with_context_and_role(
        &self,
        name: &str,
        input: Value,
        role: Role,
        tool_context: ToolContext,
    ) -> Result<Value, OrbitError> {
        let _ = (name, input, role, tool_context);
        Err(unsupported_runtime_capability(
            "run_tool_with_context_and_role",
        ))
    }
    /// Execute an engine-private VCS/PR operation for deterministic shipment.
    ///
    /// Unlike `run_tool_with_context_and_role`, this boundary never consults
    /// the public Tool registry, public authorization, or an activity
    /// allowlist. Tests override it with an in-memory fake.
    fn run_private_vcs_operation(
        &self,
        operation: &str,
        input: Value,
    ) -> Result<Value, OrbitError> {
        crate::executor::automation::vcs::run_private_operation(operation, &input)
    }
    fn resolved_agent_model_pair(&self, agent_cli: &str) -> Option<AgentModelPair> {
        let _ = agent_cli;
        None
    }
    fn scoring_enabled(&self) -> bool {
        false
    }
    /// Return the current agent model identity when this runtime is operating
    /// as an agent, or `None` when there is no model-bearing actor.
    fn actor_model_identity(&self) -> Option<String> {
        None
    }
    fn pr_config(&self) -> PrConfig {
        PrConfig::default()
    }
    fn scoreboard_dir(&self) -> &Path {
        Path::new("")
    }

    /// Dispatch a deterministic action by name. The host looks up `action`
    /// in its registry and returns the action's structured output.
    fn run_deterministic(
        &self,
        action: &str,
        config: &Value,
        input: &Value,
        tool_context: ToolContext,
    ) -> Result<Value, DispatchError> {
        let _ = (config, input, tool_context);
        Err(unsupported_dispatch_capability(action))
    }

    /// Report whether `action` names a deterministic action this host's
    /// registry can actually dispatch.
    ///
    /// [ORB-10385] Catalog assets and the installed binary are separate
    /// artifacts: a workspace can load an activity whose `action:` the running
    /// runtime does not implement. Job validation consults this before the
    /// first step runs, so that skew fails admission instead of being
    /// discovered by a terminal failure hook after a task was admitted and
    /// implemented. The default is `true`: hosts that cannot enumerate their
    /// registry (tests, smoke examples) keep the pre-ORB-10385 behavior of
    /// surfacing the miss at dispatch as
    /// [`DispatchError::DeterministicActionNotRegistered`]. Reporting `true`
    /// for an unknown action is therefore safe; reporting `false` for a
    /// dispatchable one would reject a healthy job.
    fn has_deterministic_action(&self, _action: &str) -> bool {
        true
    }

    /// Resolve the CLI executor command and static args for a given v2
    /// provider name. Workspace / env overrides live
    /// in the host so the engine's CLI runner stays environment-agnostic.
    /// Returning an error is the structured failure path when a provider has no
    /// CLI mapping (e.g. `openai_compat` which is HTTP-only).
    fn resolve_cli_executor(&self, provider: &str) -> Result<ResolvedCliExecutor, DispatchError> {
        Err(unsupported_dispatch_capability(provider))
    }

    /// Resolve the registered `local_shell` executor definition backing a
    /// deterministic shell step [ORB-11294].
    ///
    /// Separate from [`RuntimeHost::resolve_cli_executor`] on purpose: that
    /// boundary resolves an *agent* provider and rejects anything that is not
    /// `direct_agent` / `agent_cli`. A shell step carries no model, prompt, or
    /// agent tool authority, so it resolves its own executor family and never
    /// borrows an agent's.
    ///
    /// The default returns the empty definition, which is what a host without
    /// an executor store should contribute: the activity's own `config` block
    /// then has to name the command outright.
    fn resolve_local_shell_executor(
        &self,
        _executor: &str,
    ) -> Result<ResolvedShellExecutor, DispatchError> {
        Ok(ResolvedShellExecutor::default())
    }

    /// Return provider-specific CLI runtime config for agent execution.
    ///
    /// Most providers ignore this today. Codex uses it for sandbox,
    /// approval-policy, and writable-directory arguments that must stay dynamic
    /// rather than living in the static executor definition.
    fn provider_cli_config(&self, _provider: &str) -> HashMap<String, String> {
        HashMap::new()
    }

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
        _provider: &str,
        _fs_profile: Option<&str>,
        _subprocess_cwd: Option<&Path>,
    ) -> Result<Option<ResolvedSandbox>, DispatchError> {
        Ok(None)
    }

    /// Optional task snapshot to embed in a CLI agent envelope.
    ///
    /// The engine keeps this as untyped JSON so orbit-core can source task data
    /// without leaking store or task-query details into orbit-engine.
    fn task_context_for_agent_input(&self, _input: &Value) -> Result<Option<Value>, DispatchError> {
        Ok(None)
    }

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
        _task_ids: &[String],
        _activity: &str,
        _disallow_list: &[String],
    ) -> Result<ResolvedActivityTools, DispatchError> {
        Err(unsupported_dispatch_capability(
            "resolve_activity_tool_denials",
        ))
    }

    /// Compose and validate the exact task-scoped tools for one agent launch.
    /// Hosts without a task/tool registry preserve the activity baseline.
    fn resolve_activity_tools(
        &self,
        _task_ids: &[String],
        baseline_tools: &[String],
    ) -> Result<ResolvedActivityTools, DispatchError> {
        Ok(ResolvedActivityTools {
            requested_tools: Vec::new(),
            effective_tools: baseline_tools.to_vec(),
            omitted_requirement_notes: Vec::new(),
        })
    }

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
    /// its compound outputs would resume with those entries missing. The
    /// default is a no-op for hosts without run storage (tests, smoke
    /// examples). Checkpoint failures are non-fatal to the run: the executor
    /// logs and continues.
    fn checkpoint_step(
        &self,
        _run_id: &str,
        _step_index: u32,
        _step_id: &str,
        _output: &Value,
        _compound_outputs: &BTreeMap<String, Value>,
    ) -> Result<(), DispatchError> {
        Ok(())
    }

    /// Persist a successful terminal failure activity separately from normal
    /// step checkpoints. The failed step remains failed; this records only the
    /// recovery evidence a later resume may authenticate.
    fn checkpoint_failure_activity(
        &self,
        _run_id: &str,
        _activity_name: &str,
        _failed_step_id: &str,
        _output: &Value,
    ) -> Result<(), DispatchError> {
        Ok(())
    }

    /// Read only this run's bounded, redacted worker log tail for built-in
    /// step and final recovery. `None` means the run has no worker log; read failures must
    /// remain distinguishable from an empty log. This host capability is
    /// internal and grants no agent-facing run observation tool.
    fn final_recovery_log_tail(&self, _run_id: &str) -> Result<Option<String>, OrbitError> {
        Err(unsupported_runtime_capability("final_recovery_log_tail"))
    }

    /// [ORB-13907] Admit a job's final recovery for a failed run, once.
    ///
    /// A host with run storage records the admission in the run's state
    /// before answering `Admitted`, so neither a crash during the activity
    /// nor any resume of the run invokes it again; it skips when final
    /// recovery is disabled (an empty `workflow.final_recovery_crews`), when
    /// the run already spent it, or when the run is no longer running. The
    /// default skips: a host without run storage cannot promise "once".
    fn admit_final_recovery(
        &self,
        _run_id: &str,
        _request: &FinalRecoveryAdmissionRequest,
    ) -> Result<FinalRecoveryAdmission, OrbitError> {
        Ok(FinalRecoveryAdmission::Skipped {
            reason: "this runtime host does not support final recovery".to_string(),
        })
    }

    /// [ORB-13907] Act on an admitted final recovery's decision: record it,
    /// and apply every decision but `resume` to the task through the
    /// deterministic applier — or, for a claimed leaf, hand it to the claim
    /// settlement instead of writing the owner's task.
    fn apply_final_recovery(
        &self,
        _run_id: &str,
        _application: &FinalRecoveryApplication,
    ) -> Result<FinalRecoveryApplied, OrbitError> {
        Err(unsupported_runtime_capability("apply_final_recovery"))
    }

    /// Reserve the host-only identity of one admitted conflict recovery of
    /// `step_id`, before its provider runs.
    ///
    /// The host assigns the attempt; the caller only carries it, in memory, to
    /// the checkpoint it stamps as `recovery_attempt`. A later reservation for
    /// the same run and step supersedes this one, so only the newest admitted
    /// recovery can be certified. Hosts without a recovery authority refuse,
    /// which stops the recovery before the provider launches.
    fn begin_rebase_recovery_attempt(
        &self,
        _run_id: &str,
        _step_id: &str,
        _scope: &RebaseRecoveryAttemptScope,
    ) -> Result<u64, DispatchError> {
        Err(DispatchError::JobExecution(
            "host does not support durable rebase recovery checkpoints".to_string(),
        ))
    }

    /// Persist an exact host-validated recovered rebase before reporting recovery
    /// success. Unlike ordinary step checkpoints, durability failure is fatal.
    /// `output` carries the attempt reserved by
    /// [`Self::begin_rebase_recovery_attempt`] as `recovery_attempt`.
    fn checkpoint_rebase_recovery(
        &self,
        _run_id: &str,
        _step_id: &str,
        _output: &Value,
    ) -> Result<(), DispatchError> {
        Err(DispatchError::JobExecution(
            "host does not support durable rebase recovery checkpoints".to_string(),
        ))
    }

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
        _run_id: &str,
        _step_id: &str,
        _checkpoint: &Value,
    ) -> Result<bool, OrbitError> {
        Ok(false)
    }

    /// Every recovery attempt this host reserved for `run_id` / `step_id`,
    /// oldest first, read from the same host-only record that assigns them.
    /// Completion counts the distinct bases a step rebased onto to bound how
    /// often it follows a moving base [ORB-14393].
    fn rebase_recovery_attempts(
        &self,
        _run_id: &str,
        _step_id: &str,
    ) -> Result<Vec<RebaseRecoveryAttemptScope>, OrbitError> {
        Err(unsupported_runtime_capability("rebase_recovery_attempts"))
    }

    fn tool_context_for_activity(
        &self,
        _run_id: Option<&str>,
        _fs_profile: Option<&str>,
        _fs_audit: Option<Arc<dyn FsAuditLogger>>,
        _proc_allowed_programs: Option<&[String]>,
    ) -> ToolContext {
        ToolContext::default()
    }

    fn persist_invocation_trace(
        &self,
        _job_run_id: &str,
        _activity_id: &str,
        _provider: &str,
        _model: Option<&str>,
        _input: &Value,
        _trace: &InvocationTrace,
    ) -> Result<(), DispatchError> {
        Ok(())
    }

    /// Return the configured system crew for a dispatch. The engine injects
    /// this value into system-activity input immediately before resolving its
    /// explicit `crew`, rather than deriving a crew from a role or provider.
    /// Hosts without a configuration layer return `None`.
    fn system_crew_for_dispatch(&self) -> Option<String> {
        None
    }

    /// Resolve the crew selected for an activity dispatch. The engine passes
    /// rendered activity input when it contains an explicit `crew`; otherwise
    /// it passes the run input so the run's resolved crew is the fallback.
    /// Hosts without a crew registry may return `None` only when no explicit
    /// crew was requested, preserving inline settings in isolated tests.
    fn agent_crew_config_for_input(
        &self,
        input: &Value,
    ) -> Result<Option<CrewConfig>, DispatchError> {
        if let Some(crew) = input
            .get("crew")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return Err(DispatchError::JobValidation(format!(
                "explicit activity crew '{crew}' cannot be resolved by this runtime host"
            )));
        }
        Ok(None)
    }
}
