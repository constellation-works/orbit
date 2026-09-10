//! The host trait boundary between the engine and its runtime/store
//! implementors, plus the task-update param types those traits consume.

use orbit_agent::AgentConfig;
use orbit_common::OrbitError;
use orbit_common::security::child_env::allowlisted_child_env;
use orbit_store::contracts::JobRunStepParams;
use orbit_store::contracts::{InvocationQuery, InvocationRecord};
use orbit_tools::{FsAuditLogger, ToolContext};
use orbit_types::identity::AgentModelPair;
use orbit_types::policy::Role;
use orbit_types::record::OrbitEvent;
use orbit_types::task::{
    ExternalRef, Task, TaskArtifact, TaskComment, TaskHistoryEntry, TaskPriority, TaskStatus,
};
use orbit_types::telemetry::InvocationTrace;
use orbit_types::workflow::activity_job::Provider;
use orbit_types::workflow::{ActivityV2, JobRun, JobRunStartOutcome, JobRunState, PipelineState};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use crate::activity_job::{
    DispatchError, ResolvedCliExecutor, ResolvedSandbox, ResolvedShellExecutor, V2AuditWriter,
};

#[derive(Debug, Clone, Default)]
pub struct TaskAutomationUpdate {
    pub status: Option<TaskStatus>,
    pub plan: Option<String>,
    /// Default `None` = leave the task's `context_files` untouched. `Some(v)`
    /// replaces the field wholesale (mirrors `TaskDocumentUpdateParams.context_files`
    /// semantics in `orbit-store`). Only set deliberately — most automation
    /// call sites should leave this at `None`.
    pub context_files: Option<Vec<String>>,
    pub external_refs: Vec<ExternalRef>,
    pub execution_summary: Option<String>,
    pub status_event: Option<String>,
    pub status_note: Option<String>,
    pub append_comments: Vec<TaskComment>,
    pub agent: Option<String>,
    pub model: Option<String>,
    pub job_run_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TaskActivityUpdate {
    pub status: TaskStatus,
    /// Status observed before the activity requested this write. The runtime
    /// compares it under the task lock so a delayed worker cannot overwrite a
    /// newer operator decision.
    pub expected_status: TaskStatus,
    pub execution_summary: Option<String>,
    pub comment: Option<String>,
    pub note: Option<String>,
    pub agent: Option<String>,
    pub model: Option<String>,
}

/// Task requirements and the resulting activity allowlist fixed at admission.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolvedActivityTools {
    pub requested_tools: Vec<String>,
    pub effective_tools: Vec<String>,
}

fn unsupported_runtime_capability(capability: &str) -> OrbitError {
    OrbitError::Execution(format!(
        "runtime host capability '{capability}' is unavailable"
    ))
}

fn unsupported_dispatch_capability(capability: &str) -> DispatchError {
    DispatchError::JobExecution(format!(
        "runtime host capability '{capability}' is unavailable"
    ))
}

/// Resolved crew assignment from `config.toml`. Each field
/// is independently optional — the resolver in
/// `crate::activity_job::crew` falls back to the inline activity value
/// for any field the config does not specify.
///
/// String fields from the on-disk crew assignment are parsed into the
/// strongly typed activity-job enums at the orbit-core boundary; an
/// unrecognized provider yields `None` for that field rather than
/// silently coercing dispatch to a wrong runtime.
/// Whether a step-recovery hook may dispatch [ORB-11332].
///
/// `Allowed` is the pre-existing behavior for runs without an operation-mode
/// admission. `Reserved` names the aggregate-budget episode the host charged
/// before dispatch, and `Denied` carries the reason recovery must not run
/// (`recovery_episodes_exhausted`, `recovery_minutes_exhausted`,
/// `grant_revoked`). The original step error stays authoritative either way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepRecoveryAdmission {
    Allowed,
    Reserved { episode: u32 },
    Denied { reason: String },
}

/// What completion observed about a reviewed candidate's managed landing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewLandingRequest {
    pub run_id: String,
    pub task_ids: Vec<String>,
    pub workspace_path: std::path::PathBuf,
    pub pr_number: String,
    pub base: String,
    pub reviewed_head_sha: String,
    /// A conditional synchronous merge returned the same commit later observed.
    pub managed_merge: bool,
    /// The merge commit the provider reported, when it reported one.
    pub landed_commit: Option<String>,
}

/// The single capability boundary between the job executor and its runtime.
///
/// Deterministic actions, task/run persistence, environment resolution, agent
/// dispatch, and audit/checkpoint hooks all cross this boundary exactly once.
pub trait RuntimeHost: Send + Sync {
    /// Optional observation hook; execution-only test hosts need no scheduler store.
    fn record_direct_landing_intent(
        &self,
        _request: &orbit_types::workflow::automation::DirectLandingRequest,
    ) -> Result<(), OrbitError> {
        Ok(())
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

    // ── Config accessors (implementors provide these) ──────────────────

    /// Returns provider-agnostic key-value configuration that is forwarded
    /// to the selected provider factory so it can decode any provider-specific
    /// settings (for example Codex reads `"sandbox"` and `"approval_policy"`).
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
    /// `[execution.env]` policy. [ORB-10917]
    fn agent_subprocess_environment(&self, required_env_vars: &[&str]) -> Vec<(String, String)> {
        allowlisted_child_env(&[], required_env_vars)
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
    fn missing_required_environment_vars(&self, _required_env_vars: &[&str]) -> Vec<String> {
        Vec::new()
    }

    // ── Default implementations (use accessors above) ──────────────────

    fn agent_config_for(
        &self,
        agent_cli: &str,
        model: Option<&str>,
    ) -> Result<AgentConfig, OrbitError> {
        let config = self.agent_provider_config();
        AgentConfig::from_cli_config(agent_cli, model, &config)
    }

    fn validate_agent_cli(&self, cli: &str, model: Option<&str>) -> Result<(), OrbitError> {
        use orbit_agent::Agent;
        let cfg = AgentConfig::cli(cli)?.with_model(model);
        let _ = Agent::new(&cfg)?;
        Ok(())
    }

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
    fn v2_runtime_host(&self) -> Result<&dyn RuntimeHost, OrbitError> {
        Err(OrbitError::Execution(
            "v2 runtime host is not available on this host".to_string(),
        ))
    }
    fn v2_activity(&self, name: &str) -> Result<ActivityV2, OrbitError> {
        Err(OrbitError::Execution(format!(
            "v2 activity '{name}' is not available on this host"
        )))
    }
    fn v2_audit_writer(&self, run_id: &str) -> Result<Arc<V2AuditWriter>, OrbitError> {
        Err(OrbitError::Execution(format!(
            "v2 audit writer is not available for run '{run_id}'"
        )))
    }
    /// Create a task capturing a job run failure, skipping creation if an open
    /// task for the same `job_id` + `error_code` combination already exists.
    /// When `agent` and `model` are provided, they are recorded on the created
    /// task so attribution reflects the actual agent that was running.
    fn maybe_create_failure_task(
        &self,
        job_id: &str,
        run_id: &str,
        error_code: &str,
        error_message: &str,
        agent: Option<&str>,
        model: Option<&str>,
    ) -> Result<(), OrbitError> {
        let _ = (job_id, run_id, error_code, error_message, agent, model);
        Ok(())
    }
    fn resolved_agent_model_pair(&self, agent_cli: &str) -> Option<AgentModelPair> {
        let _ = agent_cli;
        None
    }
    fn canonical_model_name(&self, _agent_cli: &str, model: Option<&str>) -> Option<String> {
        model
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
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
        })
    }

    /// Persist a durable checkpoint after a completed top-level job step
    /// (ORB-10002). `pipeline_snapshot` is the executor's accumulated
    /// step-output map (step id → raw output) at the moment the step
    /// finished; `output` is the completing step's own raw output.
    ///
    /// Hosts with run persistence (orbit-core) record this into the run's
    /// `PipelineState` so an interrupted run can be resumed without
    /// re-executing completed steps. The default is a no-op for hosts
    /// without run storage (tests, smoke examples). Checkpoint failures are
    /// non-fatal to the run: the executor logs and continues.
    fn checkpoint_step(
        &self,
        _run_id: &str,
        _step_index: u32,
        _step_id: &str,
        _output: &Value,
        _pipeline_snapshot: &Value,
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

    /// Persist an exact host-validated recovered rebase before reporting recovery
    /// success. Unlike ordinary step checkpoints, durability failure is fatal.
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

    /// Whether `checkpoint` is exactly the recovery evidence this host
    /// certified for `run_id` / `step_id`.
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CrewConfig {
    pub provider: Option<Provider>,
    pub model: Option<String>,
    pub reasoning_effort: Option<orbit_types::identity::ReasoningEffort>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrConfig {
    pub task_url_template: Option<String>,
}
