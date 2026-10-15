//! Test-only null host: every [`RuntimeHost`] capability with the answer of a
//! host that lacks it.

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

use super::RuntimeHost;
use super::{
    ClaimExecutionContext, CrewConfig, FinalRecoveryAdmission, FinalRecoveryAdmissionRequest,
    FinalRecoveryApplication, FinalRecoveryApplied, HandoffLandingContext, HandoffLandingUpdate,
    PluginBrokerHandle, PluginBrokerRun, PrAutoFixAdmission, PrAutoFixRequest, PrConfig,
    RebaseRecoveryAttemptScope, ResolvedActivityTools, ReviewLandingRequest, ReviewReleaseRequest,
    ReviewReportCorrectionRequest, ReviewerInvocationRequest, ScratchGcReport,
    StepRecoveryAdmission, StepRecoveryDecisionRead, StepRecoveryDecisionRequest,
    StepRecoveryDecisionSlot, TaskActivityUpdate, TaskAutomationUpdate, WorktreeGcTaskLookup,
};

/// A [`RuntimeHost`] base for test doubles.
///
/// `RuntimeHost` declares no defaults, so the production host must name every
/// capability. A test double implements this trait instead, overriding only
/// what its test exercises; the blanket impl below makes it a `RuntimeHost`.
/// Each default is the answer of a host without that capability: a no-op, an
/// empty read, or an `unsupported` refusal. Two are live: `host_os` reports
/// this process's OS and `run_private_vcs_operation` runs the real `git` and
/// `gh` operations. Available only to tests, through
/// `orbit_engine::test_support`.
pub trait NullRuntimeHost: Send + Sync {
    fn start_plugin_broker(
        &self,
        _run: &PluginBrokerRun,
    ) -> Result<Option<Box<dyn PluginBrokerHandle>>, OrbitError> {
        Ok(None)
    }

    fn register_worker_pid_namespace(&self, _pid: u32) -> Result<(), OrbitError> {
        if NullRuntimeHost::worker_invocation(self).is_some() {
            return Err(OrbitError::Execution(
                "worker namespace authority unavailable".into(),
            ));
        }
        Ok(())
    }

    fn register_worker_process(&self, _pid: u32) -> Result<(), OrbitError> {
        if NullRuntimeHost::worker_invocation(self).is_some() {
            return Err(OrbitError::Execution(
                "worker process authority unavailable".into(),
            ));
        }
        Ok(())
    }

    fn worker_invocation(&self) -> Option<orbit_types::tool::WorkerInvocation> {
        None
    }
    fn record_direct_landing_intent(
        &self,
        _request: &orbit_types::workflow::automation::DirectLandingRequest,
    ) -> Result<(), OrbitError> {
        Ok(())
    }
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
    fn get_task_artifact(
        &self,
        _task_id: &str,
        _path: &str,
    ) -> Result<Option<TaskArtifact>, OrbitError> {
        Ok(None)
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
    fn list_run_tasks(&self, run_id: &str) -> Result<Vec<Task>, OrbitError> {
        NullRuntimeHost::list_tasks_filtered(self, None, None, None, Some(run_id), None, None)
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

    // ── Step-recovery hooks [ORB-11332] ────────────────────────────────

    fn authorize_step_recovery(
        &self,
        _run_id: &str,
        _step_id: &str,
    ) -> Result<StepRecoveryAdmission, OrbitError> {
        Ok(StepRecoveryAdmission::Allowed)
    }
    fn allocate_step_recovery_decision(
        &self,
        _request: &StepRecoveryDecisionRequest,
    ) -> Result<Option<StepRecoveryDecisionSlot>, OrbitError> {
        Ok(None)
    }
    fn read_step_recovery_decision(
        &self,
        _slot: &StepRecoveryDecisionSlot,
    ) -> Result<StepRecoveryDecisionRead, OrbitError> {
        Err(unsupported_runtime_capability(
            "read_step_recovery_decision",
        ))
    }
    fn source_repair_admission(
        &self,
        _run_id: &str,
    ) -> Result<Option<orbit_types::workflow::SourceRepairAdmission>, OrbitError> {
        Ok(None)
    }
    fn validate_source_repair(&self, _run_id: &str) -> Result<(), OrbitError> {
        Err(OrbitError::CapabilityDenied(
            "operator source repair is unavailable on this host".into(),
        ))
    }
    fn admit_pr_auto_fix(
        &self,
        _request: &PrAutoFixRequest,
    ) -> Result<PrAutoFixAdmission, OrbitError> {
        Ok(PrAutoFixAdmission::Refused(
            "this host keeps no auto-fix ledger".to_string(),
        ))
    }
    fn admit_pr_infrastructure(
        &self,
        _execution: &str,
        _task_ids: &[String],
        _evidence: &orbit_types::workflow::PrInfrastructureEvidence,
    ) -> Result<orbit_types::workflow::PrInfrastructureAdmission, OrbitError> {
        Ok(orbit_types::workflow::PrInfrastructureAdmission::Refused(
            "this host keeps no infrastructure ledger".into(),
        ))
    }
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

    // ── Before-PR review coverage [ORB-11333] ───────────────────────────

    fn record_review_landing(&self, _request: &ReviewLandingRequest) -> Result<(), OrbitError> {
        Ok(())
    }

    fn release_review_attempt(&self, _request: &ReviewReleaseRequest) -> Result<(), OrbitError> {
        Ok(())
    }

    fn record_reviewer_invocation(
        &self,
        _request: &ReviewerInvocationRequest,
    ) -> Result<Option<u64>, OrbitError> {
        Ok(None)
    }

    fn review_report_correction(
        &self,
        _request: &ReviewReportCorrectionRequest,
    ) -> Result<Option<String>, OrbitError> {
        Ok(None)
    }

    // ── Owner landing consumer [ORB-12499] ─────────────────────────────

    fn handoff_landing_context(
        &self,
        _handoff_id: &str,
    ) -> Result<HandoffLandingContext, OrbitError> {
        Err(unsupported_runtime_capability("handoff_landing_context"))
    }

    fn record_handoff_landing(&self, _update: &HandoffLandingUpdate) -> Result<(), OrbitError> {
        Err(unsupported_runtime_capability("record_handoff_landing"))
    }

    // ── Claimed distributed leaf execution [ORB-12616] ─────────────────

    fn local_machine_id(&self) -> Option<String> {
        None
    }

    fn kept_claim_candidate(
        &self,
        _task_id: &str,
    ) -> Result<Option<KeptClaimCandidate>, OrbitError> {
        Ok(None)
    }

    fn claim_execution_context(&self) -> Result<ClaimExecutionContext, OrbitError> {
        Err(unsupported_runtime_capability("claim_execution_context"))
    }

    fn attach_claim_validation_log(
        &self,
        _path: &str,
        _content: Vec<u8>,
    ) -> Result<(), OrbitError> {
        Err(unsupported_runtime_capability(
            "attach_claim_validation_log",
        ))
    }

    fn record_claim_handoff(
        &self,
        _handoff: &orbit_types::workflow::handoff::TaskHandoff,
    ) -> Result<(), OrbitError> {
        Err(unsupported_runtime_capability("record_claim_handoff"))
    }

    fn attach_task_validation_log(
        &self,
        _task_id: &str,
        _run_id: &str,
        _path: &str,
        _content: Vec<u8>,
    ) -> Result<(), OrbitError> {
        Err(unsupported_runtime_capability("attach_task_validation_log"))
    }

    fn record_flaky_validation(
        &self,
        _task_id: &str,
        _record: &orbit_types::workflow::FlakyValidation,
    ) -> Result<(), OrbitError> {
        Err(unsupported_runtime_capability("record_flaky_validation"))
    }

    // ── Config accessors (implementors provide these) ──────────────────

    fn required_validation_commands(&self) -> Vec<String> {
        Vec::new()
    }

    fn review_baseline_commands(&self) -> Vec<String> {
        Vec::new()
    }

    fn validation_gates(&self) -> orbit_types::workflow::HostValidationGates {
        orbit_types::workflow::HostValidationGates::local(orbit_types::workflow::ValidationGates {
            required_validation_commands: NullRuntimeHost::required_validation_commands(self),
            baseline_commands: NullRuntimeHost::review_baseline_commands(self),
        })
    }

    fn host_os(&self) -> Option<orbit_types::workflow::EvidenceHostOs> {
        orbit_types::workflow::EvidenceHostOs::current()
    }

    fn agent_provider_config(&self) -> HashMap<String, String> {
        HashMap::new()
    }
    fn agent_subprocess_environment(&self, required_env_vars: &[&str]) -> Vec<(String, String)> {
        allowlisted_child_env(&[], required_env_vars)
    }
    fn requires_claude_worker_credential(&self) -> bool {
        false
    }
    fn validation_subprocess_environment(&self) -> orbit_exec::ValidationEnvironment {
        orbit_exec::ValidationEnvironment::launcher(NullRuntimeHost::agent_subprocess_environment(
            self,
            &[],
        ))
    }
    fn orbit_registry_root(&self) -> Option<String> {
        None
    }
    fn orbit_workspace_selector(&self) -> Option<String> {
        None
    }
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
    fn worktree_reclaim_patterns(&self) -> Vec<String> {
        ["target", ".orbit/tmp/*-target", ".orbit/tmp/base-wt"]
            .map(String::from)
            .to_vec()
    }
    fn lookup_task_for_worktree_gc(&self, _run_id: &str, task_id: &str) -> WorktreeGcTaskLookup {
        match NullRuntimeHost::get_task(self, task_id) {
            Ok(task) => WorktreeGcTaskLookup::Found {
                status: task.status,
                pr_status: task.pr_status,
            },
            Err(_) => WorktreeGcTaskLookup::Unresolved,
        }
    }
    fn worktree_gc_task_lookup_scope(&self, _run_id: &str) -> Option<String> {
        Some("local".to_string())
    }
    fn settled_claim_for_worktree_gc(&self, _run_id: &str) -> Option<String> {
        None
    }
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
    fn actor_model_identity(&self) -> Option<String> {
        None
    }
    fn pr_config(&self) -> PrConfig {
        PrConfig::default()
    }

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

    fn has_deterministic_action(&self, _action: &str) -> bool {
        true
    }

    fn resolve_cli_executor(&self, provider: &str) -> Result<ResolvedCliExecutor, DispatchError> {
        Err(unsupported_dispatch_capability(provider))
    }

    fn resolve_local_shell_executor(
        &self,
        _executor: &str,
    ) -> Result<ResolvedShellExecutor, DispatchError> {
        Ok(ResolvedShellExecutor::default())
    }

    fn provider_cli_config(&self, _provider: &str) -> HashMap<String, String> {
        HashMap::new()
    }

    fn resolve_executor_sandbox(
        &self,
        _provider: &str,
        _fs_profile: Option<&str>,
        _subprocess_cwd: Option<&Path>,
    ) -> Result<Option<ResolvedSandbox>, DispatchError> {
        Ok(None)
    }

    fn run_deferral_probe(
        &self,
        probe: &DeferralProbe,
        _provider: Option<&str>,
        _fs_profile: Option<&str>,
        _cwd: &Path,
    ) -> NamespaceProbe {
        NamespaceProbe {
            command: probe.command.to_string(),
            exit_code: -1,
            output: "the probe could not run: this host does not support deferral probes"
                .to_string(),
        }
    }

    fn task_context_for_agent_input(&self, _input: &Value) -> Result<Option<Value>, DispatchError> {
        Ok(None)
    }

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

    fn checkpoint_step_diagnostic(
        &self,
        _run_id: &str,
        _step_id: &str,
        _output: &Value,
    ) -> Result<(), DispatchError> {
        Ok(())
    }

    fn checkpoint_failure_activity(
        &self,
        _run_id: &str,
        _activity_name: &str,
        _failed_step_id: &str,
        _output: &Value,
    ) -> Result<(), DispatchError> {
        Ok(())
    }

    fn final_recovery_log_tail(&self, _run_id: &str) -> Result<Option<String>, OrbitError> {
        Err(unsupported_runtime_capability("final_recovery_log_tail"))
    }

    fn admit_final_recovery(
        &self,
        _run_id: &str,
        _request: &FinalRecoveryAdmissionRequest,
    ) -> Result<FinalRecoveryAdmission, OrbitError> {
        Ok(FinalRecoveryAdmission::Skipped {
            reason: "this runtime host does not support final recovery".to_string(),
        })
    }

    fn apply_final_recovery(
        &self,
        _run_id: &str,
        _application: &FinalRecoveryApplication,
    ) -> Result<FinalRecoveryApplied, OrbitError> {
        Err(unsupported_runtime_capability("apply_final_recovery"))
    }

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

    fn verify_rebase_recovery(
        &self,
        _run_id: &str,
        _step_id: &str,
        _checkpoint: &Value,
    ) -> Result<bool, OrbitError> {
        Ok(false)
    }

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

impl<T: NullRuntimeHost> RuntimeHost for T {
    fn start_plugin_broker(
        &self,
        run: &PluginBrokerRun,
    ) -> Result<Option<Box<dyn PluginBrokerHandle>>, OrbitError> {
        NullRuntimeHost::start_plugin_broker(self, run)
    }
    fn register_worker_pid_namespace(&self, pid: u32) -> Result<(), OrbitError> {
        NullRuntimeHost::register_worker_pid_namespace(self, pid)
    }
    fn register_worker_process(&self, pid: u32) -> Result<(), OrbitError> {
        NullRuntimeHost::register_worker_process(self, pid)
    }
    fn worker_invocation(&self) -> Option<orbit_types::tool::WorkerInvocation> {
        NullRuntimeHost::worker_invocation(self)
    }
    fn record_direct_landing_intent(
        &self,
        request: &orbit_types::workflow::automation::DirectLandingRequest,
    ) -> Result<(), OrbitError> {
        NullRuntimeHost::record_direct_landing_intent(self, request)
    }
    fn accepted_automation_coverage(
        &self,
        task_id: &str,
    ) -> Result<Option<orbit_types::workflow::automation::CoverageEvidence>, OrbitError> {
        NullRuntimeHost::accepted_automation_coverage(self, task_id)
    }
    fn insert_job_run(
        &self,
        job_id: &str,
        attempt: u32,
        scheduled_at: chrono::DateTime<chrono::Utc>,
        input: Option<serde_json::Value>,
        retry_source_run_id: Option<String>,
    ) -> Result<JobRun, OrbitError> {
        NullRuntimeHost::insert_job_run(
            self,
            job_id,
            attempt,
            scheduled_at,
            input,
            retry_source_run_id,
        )
    }
    fn mark_job_run_running(
        &self,
        run_id: &str,
        started_at: chrono::DateTime<chrono::Utc>,
        pid: u32,
    ) -> Result<JobRunStartOutcome, OrbitError> {
        NullRuntimeHost::mark_job_run_running(self, run_id, started_at, pid)
    }
    fn complete_job_run_step(
        &self,
        run_id: &str,
        params: &JobRunStepParams,
    ) -> Result<bool, OrbitError> {
        NullRuntimeHost::complete_job_run_step(self, run_id, params)
    }
    fn finalize_job_run(
        &self,
        run_id: &str,
        state: JobRunState,
        finished_at: chrono::DateTime<chrono::Utc>,
        duration_ms: Option<u64>,
    ) -> Result<bool, OrbitError> {
        NullRuntimeHost::finalize_job_run(self, run_id, state, finished_at, duration_ms)
    }
    fn get_job_run(&self, run_id: &str) -> Result<Option<JobRun>, OrbitError> {
        NullRuntimeHost::get_job_run(self, run_id)
    }
    fn read_run_state(&self, run_id: &str) -> Result<Option<PipelineState>, OrbitError> {
        NullRuntimeHost::read_run_state(self, run_id)
    }
    fn write_run_state(&self, run_id: &str, state: &PipelineState) -> Result<(), OrbitError> {
        NullRuntimeHost::write_run_state(self, run_id, state)
    }
    fn get_task(&self, task_id: &str) -> Result<Task, OrbitError> {
        NullRuntimeHost::get_task(self, task_id)
    }
    fn get_task_artifact(
        &self,
        task_id: &str,
        path: &str,
    ) -> Result<Option<TaskArtifact>, OrbitError> {
        NullRuntimeHost::get_task_artifact(self, task_id, path)
    }
    fn get_task_artifacts(&self, task_id: &str) -> Result<Vec<TaskArtifact>, OrbitError> {
        NullRuntimeHost::get_task_artifacts(self, task_id)
    }
    fn get_task_comments(&self, task_id: &str) -> Result<Vec<TaskComment>, OrbitError> {
        NullRuntimeHost::get_task_comments(self, task_id)
    }
    fn get_task_history(&self, task_id: &str) -> Result<Vec<TaskHistoryEntry>, OrbitError> {
        NullRuntimeHost::get_task_history(self, task_id)
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
        NullRuntimeHost::list_tasks_filtered(
            self,
            status,
            priority,
            parent_id,
            job_run_id,
            external_ref,
            has_external_ref_system,
        )
    }
    fn list_run_tasks(&self, run_id: &str) -> Result<Vec<Task>, OrbitError> {
        NullRuntimeHost::list_run_tasks(self, run_id)
    }
    fn start_task(
        &self,
        task_id: &str,
        note: Option<String>,
        comment: Option<String>,
    ) -> Result<Task, OrbitError> {
        NullRuntimeHost::start_task(self, task_id, note, comment)
    }
    fn admit_task_for_workflow(&self, task_id: &str, workflow: &str) -> Result<Task, OrbitError> {
        NullRuntimeHost::admit_task_for_workflow(self, task_id, workflow)
    }
    fn update_task_from_activity(
        &self,
        task_id: &str,
        update: TaskActivityUpdate,
    ) -> Result<Task, OrbitError> {
        NullRuntimeHost::update_task_from_activity(self, task_id, update)
    }
    fn apply_task_automation_update(
        &self,
        task_id: &str,
        update: TaskAutomationUpdate,
    ) -> Result<(), OrbitError> {
        NullRuntimeHost::apply_task_automation_update(self, task_id, update)
    }
    fn widen_task_context_files(
        &self,
        task_id: &str,
        run_id: &str,
        step: ContextWideningStep,
        activity: &str,
        paths: &[String],
    ) -> Result<Vec<String>, OrbitError> {
        NullRuntimeHost::widen_task_context_files(self, task_id, run_id, step, activity, paths)
    }
    fn authorize_step_recovery(
        &self,
        run_id: &str,
        step_id: &str,
    ) -> Result<StepRecoveryAdmission, OrbitError> {
        NullRuntimeHost::authorize_step_recovery(self, run_id, step_id)
    }
    fn allocate_step_recovery_decision(
        &self,
        request: &StepRecoveryDecisionRequest,
    ) -> Result<Option<StepRecoveryDecisionSlot>, OrbitError> {
        NullRuntimeHost::allocate_step_recovery_decision(self, request)
    }
    fn read_step_recovery_decision(
        &self,
        slot: &StepRecoveryDecisionSlot,
    ) -> Result<StepRecoveryDecisionRead, OrbitError> {
        NullRuntimeHost::read_step_recovery_decision(self, slot)
    }
    fn source_repair_admission(
        &self,
        run_id: &str,
    ) -> Result<Option<orbit_types::workflow::SourceRepairAdmission>, OrbitError> {
        NullRuntimeHost::source_repair_admission(self, run_id)
    }
    fn validate_source_repair(&self, run_id: &str) -> Result<(), OrbitError> {
        NullRuntimeHost::validate_source_repair(self, run_id)
    }
    fn admit_pr_auto_fix(
        &self,
        request: &PrAutoFixRequest,
    ) -> Result<PrAutoFixAdmission, OrbitError> {
        NullRuntimeHost::admit_pr_auto_fix(self, request)
    }
    fn admit_pr_infrastructure(
        &self,
        execution: &str,
        task_ids: &[String],
        evidence: &orbit_types::workflow::PrInfrastructureEvidence,
    ) -> Result<orbit_types::workflow::PrInfrastructureAdmission, OrbitError> {
        NullRuntimeHost::admit_pr_infrastructure(self, execution, task_ids, evidence)
    }

    fn validate_step_recovery_mutation(
        &self,
        run_id: &str,
        step_id: &str,
        task_ids: &[String],
        workspace_path: &Path,
    ) -> Result<(), OrbitError> {
        NullRuntimeHost::validate_step_recovery_mutation(
            self,
            run_id,
            step_id,
            task_ids,
            workspace_path,
        )
    }
    fn record_review_landing(&self, request: &ReviewLandingRequest) -> Result<(), OrbitError> {
        NullRuntimeHost::record_review_landing(self, request)
    }
    fn release_review_attempt(&self, request: &ReviewReleaseRequest) -> Result<(), OrbitError> {
        NullRuntimeHost::release_review_attempt(self, request)
    }
    fn record_reviewer_invocation(
        &self,
        request: &ReviewerInvocationRequest,
    ) -> Result<Option<u64>, OrbitError> {
        NullRuntimeHost::record_reviewer_invocation(self, request)
    }
    fn review_report_correction(
        &self,
        request: &ReviewReportCorrectionRequest,
    ) -> Result<Option<String>, OrbitError> {
        NullRuntimeHost::review_report_correction(self, request)
    }
    fn handoff_landing_context(
        &self,
        handoff_id: &str,
    ) -> Result<HandoffLandingContext, OrbitError> {
        NullRuntimeHost::handoff_landing_context(self, handoff_id)
    }
    fn record_handoff_landing(&self, update: &HandoffLandingUpdate) -> Result<(), OrbitError> {
        NullRuntimeHost::record_handoff_landing(self, update)
    }
    fn local_machine_id(&self) -> Option<String> {
        NullRuntimeHost::local_machine_id(self)
    }
    fn kept_claim_candidate(
        &self,
        task_id: &str,
    ) -> Result<Option<KeptClaimCandidate>, OrbitError> {
        NullRuntimeHost::kept_claim_candidate(self, task_id)
    }
    fn claim_execution_context(&self) -> Result<ClaimExecutionContext, OrbitError> {
        NullRuntimeHost::claim_execution_context(self)
    }
    fn attach_claim_validation_log(&self, path: &str, content: Vec<u8>) -> Result<(), OrbitError> {
        NullRuntimeHost::attach_claim_validation_log(self, path, content)
    }
    fn record_claim_handoff(
        &self,
        handoff: &orbit_types::workflow::handoff::TaskHandoff,
    ) -> Result<(), OrbitError> {
        NullRuntimeHost::record_claim_handoff(self, handoff)
    }
    fn attach_task_validation_log(
        &self,
        task_id: &str,
        run_id: &str,
        path: &str,
        content: Vec<u8>,
    ) -> Result<(), OrbitError> {
        NullRuntimeHost::attach_task_validation_log(self, task_id, run_id, path, content)
    }
    fn record_flaky_validation(
        &self,
        task_id: &str,
        record: &orbit_types::workflow::FlakyValidation,
    ) -> Result<(), OrbitError> {
        NullRuntimeHost::record_flaky_validation(self, task_id, record)
    }
    fn required_validation_commands(&self) -> Vec<String> {
        NullRuntimeHost::required_validation_commands(self)
    }
    fn review_baseline_commands(&self) -> Vec<String> {
        NullRuntimeHost::review_baseline_commands(self)
    }
    fn validation_gates(&self) -> orbit_types::workflow::HostValidationGates {
        NullRuntimeHost::validation_gates(self)
    }
    fn host_os(&self) -> Option<orbit_types::workflow::EvidenceHostOs> {
        NullRuntimeHost::host_os(self)
    }
    fn agent_provider_config(&self) -> HashMap<String, String> {
        NullRuntimeHost::agent_provider_config(self)
    }
    fn agent_subprocess_environment(&self, required_env_vars: &[&str]) -> Vec<(String, String)> {
        NullRuntimeHost::agent_subprocess_environment(self, required_env_vars)
    }
    fn requires_claude_worker_credential(&self) -> bool {
        NullRuntimeHost::requires_claude_worker_credential(self)
    }
    fn validation_subprocess_environment(&self) -> orbit_exec::ValidationEnvironment {
        NullRuntimeHost::validation_subprocess_environment(self)
    }
    fn orbit_registry_root(&self) -> Option<String> {
        NullRuntimeHost::orbit_registry_root(self)
    }
    fn orbit_workspace_selector(&self) -> Option<String> {
        NullRuntimeHost::orbit_workspace_selector(self)
    }
    fn refresh_persistence_after_cli_provider(&self) -> Result<(), OrbitError> {
        NullRuntimeHost::refresh_persistence_after_cli_provider(self)
    }
    fn record_event(&self, event: OrbitEvent) -> Result<(), OrbitError> {
        NullRuntimeHost::record_event(self, event)
    }
    fn repo_root(&self) -> Result<String, OrbitError> {
        NullRuntimeHost::repo_root(self)
    }
    fn list_job_runs_for_gc(&self) -> Result<Vec<JobRun>, OrbitError> {
        NullRuntimeHost::list_job_runs_for_gc(self)
    }
    fn worktree_reclaim_patterns(&self) -> Vec<String> {
        NullRuntimeHost::worktree_reclaim_patterns(self)
    }
    fn lookup_task_for_worktree_gc(&self, run_id: &str, task_id: &str) -> WorktreeGcTaskLookup {
        NullRuntimeHost::lookup_task_for_worktree_gc(self, run_id, task_id)
    }
    fn worktree_gc_task_lookup_scope(&self, run_id: &str) -> Option<String> {
        NullRuntimeHost::worktree_gc_task_lookup_scope(self, run_id)
    }
    fn settled_claim_for_worktree_gc(&self, run_id: &str) -> Option<String> {
        NullRuntimeHost::settled_claim_for_worktree_gc(self, run_id)
    }
    fn gc_scratch(&self, retention_hours: u64) -> Result<Option<ScratchGcReport>, OrbitError> {
        NullRuntimeHost::gc_scratch(self, retention_hours)
    }
    fn data_root(&self) -> &Path {
        NullRuntimeHost::data_root(self)
    }
    fn cancel_job_run(&self, run_id: &str) -> Result<(), OrbitError> {
        NullRuntimeHost::cancel_job_run(self, run_id)
    }
    fn invocation_records(
        &self,
        query: InvocationQuery,
    ) -> Result<Vec<InvocationRecord>, OrbitError> {
        NullRuntimeHost::invocation_records(self, query)
    }
    fn record_provider_limit(
        &self,
        observation: &ProviderLimitObservation,
    ) -> Result<(), OrbitError> {
        NullRuntimeHost::record_provider_limit(self, observation)
    }
    fn activity_implementer_identity(
        &self,
        input: &Value,
    ) -> Result<(Option<String>, Option<String>), OrbitError> {
        NullRuntimeHost::activity_implementer_identity(self, input)
    }
    fn resolved_crew_model(&self, run_id: &str) -> Result<Option<String>, OrbitError> {
        NullRuntimeHost::resolved_crew_model(self, run_id)
    }
    fn run_tool_with_context_and_role(
        &self,
        name: &str,
        input: Value,
        role: Role,
        tool_context: ToolContext,
    ) -> Result<Value, OrbitError> {
        NullRuntimeHost::run_tool_with_context_and_role(self, name, input, role, tool_context)
    }
    fn run_private_vcs_operation(
        &self,
        operation: &str,
        input: Value,
    ) -> Result<Value, OrbitError> {
        NullRuntimeHost::run_private_vcs_operation(self, operation, input)
    }
    fn resolved_agent_model_pair(&self, agent_cli: &str) -> Option<AgentModelPair> {
        NullRuntimeHost::resolved_agent_model_pair(self, agent_cli)
    }
    fn actor_model_identity(&self) -> Option<String> {
        NullRuntimeHost::actor_model_identity(self)
    }
    fn pr_config(&self) -> PrConfig {
        NullRuntimeHost::pr_config(self)
    }
    fn run_deterministic(
        &self,
        action: &str,
        config: &Value,
        input: &Value,
        tool_context: ToolContext,
    ) -> Result<Value, DispatchError> {
        NullRuntimeHost::run_deterministic(self, action, config, input, tool_context)
    }
    fn has_deterministic_action(&self, action: &str) -> bool {
        NullRuntimeHost::has_deterministic_action(self, action)
    }
    fn resolve_cli_executor(&self, provider: &str) -> Result<ResolvedCliExecutor, DispatchError> {
        NullRuntimeHost::resolve_cli_executor(self, provider)
    }
    fn resolve_local_shell_executor(
        &self,
        executor: &str,
    ) -> Result<ResolvedShellExecutor, DispatchError> {
        NullRuntimeHost::resolve_local_shell_executor(self, executor)
    }
    fn provider_cli_config(&self, provider: &str) -> HashMap<String, String> {
        NullRuntimeHost::provider_cli_config(self, provider)
    }
    fn resolve_executor_sandbox(
        &self,
        provider: &str,
        fs_profile: Option<&str>,
        subprocess_cwd: Option<&Path>,
    ) -> Result<Option<ResolvedSandbox>, DispatchError> {
        NullRuntimeHost::resolve_executor_sandbox(self, provider, fs_profile, subprocess_cwd)
    }
    fn run_deferral_probe(
        &self,
        probe: &DeferralProbe,
        provider: Option<&str>,
        fs_profile: Option<&str>,
        cwd: &Path,
    ) -> NamespaceProbe {
        NullRuntimeHost::run_deferral_probe(self, probe, provider, fs_profile, cwd)
    }
    fn task_context_for_agent_input(&self, input: &Value) -> Result<Option<Value>, DispatchError> {
        NullRuntimeHost::task_context_for_agent_input(self, input)
    }
    fn resolve_activity_tool_denials(
        &self,
        task_ids: &[String],
        activity: &str,
        disallow_list: &[String],
    ) -> Result<ResolvedActivityTools, DispatchError> {
        NullRuntimeHost::resolve_activity_tool_denials(self, task_ids, activity, disallow_list)
    }
    fn resolve_activity_tools(
        &self,
        task_ids: &[String],
        baseline_tools: &[String],
    ) -> Result<ResolvedActivityTools, DispatchError> {
        NullRuntimeHost::resolve_activity_tools(self, task_ids, baseline_tools)
    }
    fn checkpoint_step(
        &self,
        run_id: &str,
        step_index: u32,
        step_id: &str,
        output: &Value,
        compound_outputs: &BTreeMap<String, Value>,
    ) -> Result<(), DispatchError> {
        NullRuntimeHost::checkpoint_step(
            self,
            run_id,
            step_index,
            step_id,
            output,
            compound_outputs,
        )
    }
    fn checkpoint_step_diagnostic(
        &self,
        run_id: &str,
        step_id: &str,
        output: &Value,
    ) -> Result<(), DispatchError> {
        NullRuntimeHost::checkpoint_step_diagnostic(self, run_id, step_id, output)
    }

    fn checkpoint_failure_activity(
        &self,
        run_id: &str,
        activity_name: &str,
        failed_step_id: &str,
        output: &Value,
    ) -> Result<(), DispatchError> {
        NullRuntimeHost::checkpoint_failure_activity(
            self,
            run_id,
            activity_name,
            failed_step_id,
            output,
        )
    }
    fn final_recovery_log_tail(&self, run_id: &str) -> Result<Option<String>, OrbitError> {
        NullRuntimeHost::final_recovery_log_tail(self, run_id)
    }
    fn admit_final_recovery(
        &self,
        run_id: &str,
        request: &FinalRecoveryAdmissionRequest,
    ) -> Result<FinalRecoveryAdmission, OrbitError> {
        NullRuntimeHost::admit_final_recovery(self, run_id, request)
    }
    fn apply_final_recovery(
        &self,
        run_id: &str,
        application: &FinalRecoveryApplication,
    ) -> Result<FinalRecoveryApplied, OrbitError> {
        NullRuntimeHost::apply_final_recovery(self, run_id, application)
    }
    fn begin_rebase_recovery_attempt(
        &self,
        run_id: &str,
        step_id: &str,
        scope: &RebaseRecoveryAttemptScope,
    ) -> Result<u64, DispatchError> {
        NullRuntimeHost::begin_rebase_recovery_attempt(self, run_id, step_id, scope)
    }
    fn checkpoint_rebase_recovery(
        &self,
        run_id: &str,
        step_id: &str,
        output: &Value,
    ) -> Result<(), DispatchError> {
        NullRuntimeHost::checkpoint_rebase_recovery(self, run_id, step_id, output)
    }
    fn verify_rebase_recovery(
        &self,
        run_id: &str,
        step_id: &str,
        checkpoint: &Value,
    ) -> Result<bool, OrbitError> {
        NullRuntimeHost::verify_rebase_recovery(self, run_id, step_id, checkpoint)
    }
    fn rebase_recovery_attempts(
        &self,
        run_id: &str,
        step_id: &str,
    ) -> Result<Vec<RebaseRecoveryAttemptScope>, OrbitError> {
        NullRuntimeHost::rebase_recovery_attempts(self, run_id, step_id)
    }
    fn tool_context_for_activity(
        &self,
        run_id: Option<&str>,
        fs_profile: Option<&str>,
        fs_audit: Option<Arc<dyn FsAuditLogger>>,
        proc_allowed_programs: Option<&[String]>,
    ) -> ToolContext {
        NullRuntimeHost::tool_context_for_activity(
            self,
            run_id,
            fs_profile,
            fs_audit,
            proc_allowed_programs,
        )
    }
    fn persist_invocation_trace(
        &self,
        job_run_id: &str,
        activity_id: &str,
        provider: &str,
        model: Option<&str>,
        input: &Value,
        trace: &InvocationTrace,
    ) -> Result<(), DispatchError> {
        NullRuntimeHost::persist_invocation_trace(
            self,
            job_run_id,
            activity_id,
            provider,
            model,
            input,
            trace,
        )
    }
    fn agent_crew_config_for_input(
        &self,
        input: &Value,
    ) -> Result<Option<CrewConfig>, DispatchError> {
        NullRuntimeHost::agent_crew_config_for_input(self, input)
    }
}
