//! `RuntimeHost` implementation for `OrbitRuntime`.
//!
//! Most methods delegate to a sibling helper grouped by concern (`claims`,
//! `validation`, `recovery`, `invocation` and the other modules declared in
//! `mod.rs`), or adapt a type on the way through, with the trait signature
//! unchanged. Rust allows one trait impl per type, so this file cannot be
//! split into multiple impl blocks.
//!
//! Three bodies hold logic rather than delegating. They stay here as residual
//! exceptions to that pattern:
//!
//! - `requires_claude_worker_credential` is a platform policy
//!   (`cfg!(target_os = "macos")`).
//! - `orbit_workspace_selector` trims the logical workspace id and maps an
//!   empty selector to `None`.
//! - `refresh_persistence_after_cli_provider` runs a store lookup with `?`
//!   and then a refresh call.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_engine::{
    CompletionObservationAdmission, CrewConfig, DispatchError, FinalRecoveryAdmission,
    FinalRecoveryAdmissionRequest, FinalRecoveryApplication, FinalRecoveryApplied,
    PluginBrokerHandle, PluginBrokerRun, RebaseRecoveryAttemptScope, ResolvedActivityTools,
    ResolvedCliExecutor, ResolvedSandbox, ResolvedShellExecutor, RuntimeHost, SteeringGateReading,
    SteeringRoundAdmission, StepRecoveryDecisionRead, StepRecoveryDecisionRequest,
    StepRecoveryDecisionSlot, TaskActivityUpdate, TaskAutomationUpdate,
};
use orbit_store::contracts::{InvocationQuery, InvocationRecord, JobRunStepParams};
use orbit_tools::{FsAuditLogger, ToolContext};
use orbit_types::identity::AgentModelPair;
use orbit_types::policy::Role;
use orbit_types::record::OrbitEvent;
use orbit_types::task::{
    ContextWideningStep, ExternalRef, Task, TaskComment, TaskHistoryEntry, TaskPriority, TaskStatus,
};
use orbit_types::telemetry::ProviderLimitObservation;
use orbit_types::workflow::{
    CommentLogMark, DeferralProbe, ExperimentActivityOverlay, ExploreBriefAdmission, JobRun,
    JobRunStartOutcome, JobRunState, NamespaceProbe, TagInstruction,
};
use serde_json::Value;

use crate::OrbitRuntime;
use crate::adapter::engine_host::actions::{cli_executor, dispatch, sandbox, task_context};
use crate::runtime::engine::paths::current_repo_root;

use super::{
    activity_tools, agent, auto_task, checkpoints, claims, crew, executors, invocation, recovery,
    task_automation, validation,
};

impl RuntimeHost for OrbitRuntime {
    fn start_plugin_broker(
        &self,
        run: &PluginBrokerRun,
    ) -> Result<Option<Box<dyn PluginBrokerHandle>>, OrbitError> {
        executors::start_plugin_broker(self, run)
    }

    fn register_worker_pid_namespace(&self, pid: u32) -> Result<(), OrbitError> {
        executors::register_worker_pid_namespace(self, pid)
    }

    fn register_worker_process(&self, pid: u32) -> Result<(), OrbitError> {
        crate::runtime::RuntimeKernel::register_worker_process(self, pid)
    }

    fn run_script_step(
        &self,
        request: &orbit_engine::ScriptStepRequest,
    ) -> orbit_engine::ScriptStepRun {
        crate::application::job::script_step::run_script_step(self, request)
    }

    fn verify_script_output_set(
        &self,
        consumer_run_id: &str,
        set: &orbit_types::workflow::activity_job::ScriptOutputSet,
    ) -> Result<(), String> {
        crate::application::job::script_outputs::verify_script_output_set(
            self,
            consumer_run_id,
            set,
        )
    }

    fn worker_invocation(&self) -> Option<orbit_types::tool::WorkerInvocation> {
        crate::runtime::RuntimeKernel::worker_invocation(self).cloned()
    }
    fn record_direct_landing_intent(
        &self,
        request: &orbit_types::workflow::automation::DirectLandingRequest,
    ) -> Result<(), OrbitError> {
        crate::application::automation::record_direct_landing_intent(self, request)
    }

    fn record_direct_landing(
        &self,
        request: &orbit_types::workflow::automation::DirectLandingRequest,
    ) -> Result<(), OrbitError> {
        claims::record_direct_landing(self, request)
    }

    fn accepted_automation_coverage(
        &self,
        task_id: &str,
    ) -> Result<Option<orbit_types::workflow::automation::CoverageEvidence>, OrbitError> {
        crate::application::automation::accepted_action_coverage(self, task_id, Utc::now())
    }

    fn insert_job_run(
        &self,
        job_id: &str,
        attempt: u32,
        scheduled_at: DateTime<Utc>,
        input: Option<serde_json::Value>,
        retry_source_run_id: Option<String>,
    ) -> Result<JobRun, OrbitError> {
        claims::insert_job_run(
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
        started_at: DateTime<Utc>,
        pid: u32,
    ) -> Result<JobRunStartOutcome, OrbitError> {
        claims::mark_job_run_running(self, run_id, started_at, pid)
    }

    fn complete_job_run_step(
        &self,
        run_id: &str,
        params: &JobRunStepParams,
    ) -> Result<bool, OrbitError> {
        claims::complete_job_run_step(self, run_id, params)
    }

    fn finalize_job_run(
        &self,
        run_id: &str,
        state: JobRunState,
        finished_at: DateTime<Utc>,
        duration_ms: Option<u64>,
    ) -> Result<bool, OrbitError> {
        claims::finalize_job_run(self, run_id, state, finished_at, duration_ms)
    }

    fn get_job_run(&self, run_id: &str) -> Result<Option<JobRun>, OrbitError> {
        claims::get_job_run(self, run_id)
    }

    fn read_run_state(
        &self,
        run_id: &str,
    ) -> Result<Option<orbit_types::workflow::PipelineState>, OrbitError> {
        claims::read_run_state(self, run_id)
    }

    fn write_run_state(
        &self,
        run_id: &str,
        state: &orbit_types::workflow::PipelineState,
    ) -> Result<(), OrbitError> {
        claims::write_run_state(self, run_id, state)
    }

    fn get_task(&self, task_id: &str) -> Result<Task, OrbitError> {
        OrbitRuntime::get_task(self, task_id)
    }

    fn get_task_artifact(
        &self,
        task_id: &str,
        path: &str,
    ) -> Result<Option<orbit_types::task::TaskArtifact>, OrbitError> {
        self.get_task_artifact_inline(task_id, path)
    }

    fn get_task_artifacts(
        &self,
        task_id: &str,
    ) -> Result<Vec<orbit_types::task::TaskArtifact>, OrbitError> {
        OrbitRuntime::get_task_artifacts(self, task_id)
    }

    fn get_task_comments(&self, task_id: &str) -> Result<Vec<TaskComment>, OrbitError> {
        OrbitRuntime::get_task_comments(self, task_id)
    }

    fn get_task_history(&self, task_id: &str) -> Result<Vec<TaskHistoryEntry>, OrbitError> {
        OrbitRuntime::get_task_history(self, task_id)
    }

    fn get_dependency_task(&self, task_id: &str) -> Result<Option<Task>, OrbitError> {
        OrbitRuntime::get_dependency_task(self, task_id)
    }

    fn get_dependency_history(
        &self,
        task_id: &str,
    ) -> Result<Option<Vec<TaskHistoryEntry>>, OrbitError> {
        OrbitRuntime::get_dependency_history(self, task_id)
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
        OrbitRuntime::list_tasks_filtered(
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
        OrbitRuntime::list_run_tasks(self, run_id)
    }

    fn start_task(
        &self,
        task_id: &str,
        note: Option<String>,
        comment: Option<String>,
    ) -> Result<Task, OrbitError> {
        OrbitRuntime::start_task_as_system(self, task_id, note, comment)
    }

    /// Admit a task into a pipeline that is about to build its worktree.
    ///
    /// A claimed leaf takes the bound branch [ORB-12616]: the owner already
    /// admitted this task inside the pull transaction, so re-admitting it
    /// locally would either be a no-op on a replica read or, worse, a second
    /// authority for the same work. Instead the trusted worker binding fixes
    /// which task this run may touch, and the owner's own copy of that task is
    /// read back to confirm it is still the admitted, in-progress one. No
    /// binding means no claimed admission: a bound-less process reaching a
    /// claimed leaf has already been refused upstream, and an unbound ordinary
    /// run keeps the pre-existing local admission unchanged.
    fn admit_task_for_workflow(&self, task_id: &str, workflow: &str) -> Result<Task, OrbitError> {
        claims::admit_task_for_workflow(self, task_id, workflow)
    }

    fn update_task_from_activity(
        &self,
        task_id: &str,
        update: TaskActivityUpdate,
    ) -> Result<Task, OrbitError> {
        claims::update_task_from_activity(self, task_id, update)
    }

    /// Orbit admits every step recovery here. Each one stays bounded by its
    /// step's attempt budget, and any Git mutation it asks for must first pass
    /// `validate_step_recovery_mutation`.
    fn authorize_step_recovery(
        &self,
        run_id: &str,
        step_id: &str,
    ) -> Result<orbit_engine::StepRecoveryAdmission, OrbitError> {
        recovery::authorize_step_recovery(self, run_id, step_id)
    }

    fn validate_step_recovery_mutation(
        &self,
        run_id: &str,
        step_id: &str,
        task_ids: &[String],
        workspace_path: &std::path::Path,
    ) -> Result<(), OrbitError> {
        recovery::validate_step_recovery_mutation(self, run_id, step_id, task_ids, workspace_path)
    }

    fn record_review_landing(
        &self,
        request: &orbit_engine::ReviewLandingRequest,
    ) -> Result<(), OrbitError> {
        crate::application::review::record_review_landing(self, request)
    }

    fn release_review_attempt(
        &self,
        request: &orbit_engine::ReviewReleaseRequest,
    ) -> Result<(), OrbitError> {
        crate::application::review::release_review_attempt(self, request)
    }

    fn record_reviewer_invocation(
        &self,
        request: &orbit_engine::ReviewerInvocationRequest,
    ) -> Result<Option<u64>, OrbitError> {
        crate::application::review::record_reviewer_invocation(self, request)
    }

    fn review_report_correction(
        &self,
        request: &orbit_engine::ReviewReportCorrectionRequest,
    ) -> Result<Option<String>, OrbitError> {
        crate::application::review::review_report_correction(self, request)
    }

    fn handoff_landing_context(
        &self,
        handoff_id: &str,
    ) -> Result<orbit_engine::HandoffLandingContext, OrbitError> {
        OrbitRuntime::handoff_landing_context(self, handoff_id)
    }

    fn record_handoff_landing(
        &self,
        update: &orbit_engine::HandoffLandingUpdate,
    ) -> Result<(), OrbitError> {
        OrbitRuntime::record_handoff_landing(self, update)
    }

    fn record_handoff_landing_wait(
        &self,
        handoff_id: &str,
        reason: &str,
    ) -> Result<(), OrbitError> {
        OrbitRuntime::record_handoff_landing_wait(self, handoff_id, reason)
    }

    fn record_handoff_landing_history(
        &self,
        handoff_id: &str,
        event: &str,
        note: &str,
    ) -> Result<(), OrbitError> {
        OrbitRuntime::record_handoff_landing_history(self, handoff_id, event, note)
    }

    fn local_machine_id(&self) -> Option<String> {
        self.automation_machine_identity().map(str::to_string)
    }

    fn kept_claim_candidate(
        &self,
        task_id: &str,
    ) -> Result<Option<orbit_store::contracts::KeptClaimCandidate>, OrbitError> {
        claims::kept_claim_candidate(self, task_id)
    }

    fn claim_execution_context(&self) -> Result<orbit_engine::ClaimExecutionContext, OrbitError> {
        claims::claim_execution_context(self)
    }

    fn attach_claim_validation_log(&self, path: &str, content: Vec<u8>) -> Result<(), OrbitError> {
        validation::attach_claim_validation_log(self, path, content)
    }

    fn attach_task_validation_log(
        &self,
        task_id: &str,
        run_id: &str,
        path: &str,
        content: Vec<u8>,
    ) -> Result<(), OrbitError> {
        validation::attach_task_validation_log(self, task_id, run_id, path, content)
    }

    fn record_flaky_validation(
        &self,
        task_id: &str,
        record: &orbit_types::workflow::FlakyValidation,
    ) -> Result<(), OrbitError> {
        validation::record_flaky_validation(self, task_id, record)
    }

    fn admit_owed_criteria(
        &self,
        task_id: &str,
        run_id: &str,
        owed: &[orbit_types::workflow::OwedCriterion],
    ) -> Result<(), OrbitError> {
        validation::admit_owed_criteria(self, task_id, run_id, owed)
    }

    fn outstanding_owed_criteria(
        &self,
        task_id: &str,
        run_id: Option<&str>,
    ) -> Result<Vec<usize>, OrbitError> {
        validation::outstanding_owed_criteria(self, task_id, run_id)
    }

    fn record_claim_handoff(
        &self,
        handoff: &orbit_types::workflow::handoff::TaskHandoff,
    ) -> Result<(), OrbitError> {
        claims::record_claim_handoff(self, handoff)
    }

    fn reject_invalid_finding(
        &self,
        report: &orbit_types::workflow::handoff::FindingInvalidReport,
        base_branch: &str,
    ) -> Result<(), OrbitError> {
        task_automation::reject_invalid_finding(self, report, base_branch)
    }

    fn widen_task_context_files(
        &self,
        task_id: &str,
        run_id: &str,
        step: ContextWideningStep,
        activity: &str,
        paths: &[String],
    ) -> Result<Vec<String>, OrbitError> {
        claims::widen_task_context_files(self, task_id, run_id, step, activity, paths)
    }

    fn apply_task_automation_update(
        &self,
        task_id: &str,
        update: TaskAutomationUpdate,
    ) -> Result<(), OrbitError> {
        task_automation::apply_task_automation_update(self, task_id, update)
    }

    fn required_validation_commands(&self) -> Vec<String> {
        validation::required_validation_commands(self)
    }

    fn review_baseline_commands(&self) -> Vec<String> {
        validation::review_baseline_commands(self)
    }

    fn workspace_base_branch(&self) -> String {
        crate::runtime::RuntimeKernel::workspace_base_branch(self).to_string()
    }

    fn validation_gates(&self) -> orbit_types::workflow::HostValidationGates {
        validation::validation_gates(self)
    }

    fn unlocked_path_validation(&self) -> orbit_engine::UnlockedPathValidation {
        validation::unlocked_path_validation(self)
    }

    fn generated_paths(&self) -> Vec<orbit_engine::GeneratedPath> {
        validation::generated_paths(self)
    }

    fn record_validated_tree(&self, tree: &orbit_engine::ValidatedTree) -> Result<(), OrbitError> {
        validation::record_validated_tree(self, tree)
    }

    fn validated_tree(
        &self,
        run_id: &str,
    ) -> Result<Option<orbit_engine::ValidatedTree>, OrbitError> {
        validation::validated_tree(self, run_id)
    }

    fn host_os(&self) -> Option<orbit_types::workflow::EvidenceHostOs> {
        orbit_types::workflow::EvidenceHostOs::current()
    }

    fn agent_stall_window(&self) -> Option<std::time::Duration> {
        validation::agent_stall_window(self)
    }

    fn agent_provider_config(&self) -> std::collections::HashMap<String, String> {
        agent::agent_provider_config(self)
    }

    fn agent_subprocess_environment(&self, required_env_vars: &[&str]) -> Vec<(String, String)> {
        self.agent_session_environment(required_env_vars)
    }

    fn crew_agent_subprocess_environment(
        &self,
        crew_env: &[orbit_types::identity::CrewEnvVar],
        required_env_vars: &[&str],
    ) -> Vec<(String, String)> {
        self.crew_agent_session_environment(crew_env, required_env_vars)
    }

    fn requires_claude_worker_credential(&self) -> bool {
        cfg!(target_os = "macos")
    }

    fn validation_subprocess_environment(&self) -> orbit_exec::ValidationEnvironment {
        self.validation_environment()
    }

    fn validation_build_output_env(&self) -> Vec<String> {
        self.validation_env_policy().build_output_env.clone()
    }

    fn orbit_registry_root(&self) -> Option<String> {
        Some(
            self.context
                .paths()
                .global_dir
                .to_string_lossy()
                .into_owned(),
        )
    }

    fn orbit_workspace_selector(&self) -> Option<String> {
        self.workspace_runtime_binding()
            .map(|binding| binding.logical_workspace_id.trim())
            .filter(|selector| !selector.is_empty())
            .map(ToOwned::to_owned)
    }

    fn refresh_persistence_after_cli_provider(&self) -> Result<(), OrbitError> {
        self.sqlite_store()?
            .refresh_file_connections(&self.context.persistence().audit_db)
    }

    fn record_event(&self, event: OrbitEvent) -> Result<(), OrbitError> {
        crate::runtime::RuntimeKernel::record_event(self, event)
    }

    fn repo_root(&self) -> Result<String, OrbitError> {
        current_repo_root(self)
    }

    fn list_job_runs_for_gc(&self) -> Result<Vec<JobRun>, OrbitError> {
        self.list_job_runs_for_worktree_gc()
    }

    fn worktree_reclaim_patterns(&self) -> Vec<String> {
        self.context.settings().worktree_reclaim().to_vec()
    }

    fn worktree_parked_reclaim_after_hours(&self) -> Option<u64> {
        Some(
            self.context
                .settings()
                .worktree_parked_reclaim_after_hours(),
        )
    }

    fn lookup_task_for_worktree_gc(
        &self,
        run_id: &str,
        task_id: &str,
    ) -> orbit_engine::WorktreeGcTaskLookup {
        self.worktree_gc_task_lookup(run_id, task_id)
    }

    fn worktree_gc_task_lookup_scope(&self, run_id: &str) -> Option<String> {
        OrbitRuntime::worktree_gc_task_lookup_scope(self, run_id)
    }

    fn settled_claim_for_worktree_gc(&self, run_id: &str) -> Option<String> {
        self.worktree_gc_settled_claim(run_id)
    }

    fn live_claim_for_worktree_gc(&self, run_id: &str) -> Option<String> {
        self.worktree_gc_live_claim(run_id)
    }

    fn gc_scratch(
        &self,
        retention_hours: u64,
    ) -> Result<Option<orbit_engine::ScratchGcReport>, OrbitError> {
        self.gc_scratch_by_age(retention_hours).map(Some)
    }

    fn data_root(&self) -> &std::path::Path {
        self.context.data_root()
    }

    fn cancel_job_run(&self, run_id: &str) -> Result<(), OrbitError> {
        OrbitRuntime::cancel_job_run(self, run_id).map(|_| ())
    }

    fn resolved_agent_model_pair(&self, agent_cli: &str) -> Option<AgentModelPair> {
        invocation::resolved_agent_model_pair(self, agent_cli)
    }

    fn invocation_records(
        &self,
        query: InvocationQuery,
    ) -> Result<Vec<InvocationRecord>, OrbitError> {
        invocation::invocation_records(self, query)
    }

    fn record_provider_limit(
        &self,
        observation: &ProviderLimitObservation,
    ) -> Result<(), OrbitError> {
        invocation::record_provider_limit(self, observation)
    }

    fn activity_implementer_identity(
        &self,
        input: &Value,
    ) -> Result<(Option<String>, Option<String>), OrbitError> {
        invocation::activity_implementer_identity(self, input)
    }

    fn resolved_crew_model(&self, run_id: &str) -> Result<Option<String>, OrbitError> {
        crew::resolved_crew_model(self, run_id)
    }

    fn run_tool_with_context_and_role(
        &self,
        name: &str,
        input: Value,
        role: Role,
        tool_context: ToolContext,
    ) -> Result<Value, OrbitError> {
        OrbitRuntime::run_tool_with_context_and_role(self, name, input, role, tool_context)
    }

    fn run_private_vcs_operation(
        &self,
        operation: &str,
        input: Value,
    ) -> Result<Value, OrbitError> {
        orbit_engine::run_private_vcs_operation(operation, &input)
    }

    fn actor_model_identity(&self) -> Option<String> {
        agent::actor_model_identity(self)
    }

    fn pr_config(&self) -> orbit_engine::PrConfig {
        crate::runtime::RuntimeKernel::pr_config(self).clone()
    }

    fn run_deterministic(
        &self,
        action: &str,
        config: &Value,
        input: &Value,
        tool_context: ToolContext,
    ) -> Result<Value, DispatchError> {
        dispatch::run_deterministic(self, action, config, input, tool_context)
    }

    /// [ORB-10385] Report this binary's deterministic-action registry so job
    /// validation can reject a catalog asset naming an action we cannot
    /// dispatch, before the run admits a task or builds a worktree.
    fn has_deterministic_action(&self, action: &str) -> bool {
        dispatch::is_deterministic_action_registered(action)
    }

    fn resolve_cli_executor(&self, provider: &str) -> Result<ResolvedCliExecutor, DispatchError> {
        cli_executor::resolve_cli_executor(self, provider)
    }

    fn resolve_local_shell_executor(
        &self,
        executor: &str,
    ) -> Result<ResolvedShellExecutor, DispatchError> {
        cli_executor::resolve_local_shell_executor(self, executor)
    }

    fn provider_cli_config(&self, provider: &str) -> HashMap<String, String> {
        invocation::provider_cli_config(self, provider)
    }

    fn resolve_executor_sandbox(
        &self,
        provider: &str,
        #[cfg(target_os = "macos")] fs_profile: Option<&str>,
        #[cfg(not(target_os = "macos"))] _fs_profile: Option<&str>,
        #[cfg(target_os = "macos")] subprocess_cwd: Option<&Path>,
        #[cfg(not(target_os = "macos"))] _subprocess_cwd: Option<&Path>,
    ) -> Result<Option<ResolvedSandbox>, DispatchError> {
        sandbox::resolve_executor_sandbox(
            self,
            provider,
            #[cfg(target_os = "macos")]
            fs_profile,
            #[cfg(not(target_os = "macos"))]
            _fs_profile,
            #[cfg(target_os = "macos")]
            subprocess_cwd,
            #[cfg(not(target_os = "macos"))]
            _subprocess_cwd,
        )
    }

    fn task_context_for_agent_input(
        &self,
        run_id: &str,
        step_id: &str,
        input: &Value,
    ) -> Result<Option<Value>, DispatchError> {
        task_context::task_context_for_agent_input(self, run_id, step_id, input)
    }

    fn regression_risk_for_agent_input(
        &self,
        run_id: &str,
        activity_name: &str,
        input: &Value,
    ) -> Option<Value> {
        task_context::regression_risk_for_agent_input(self, run_id, activity_name, input)
    }

    fn agent_corpus_for_activity(
        &self,
        run_id: &str,
        activity_name: &str,
    ) -> Result<Option<orbit_types::workflow::AgentCorpusInjection>, DispatchError> {
        OrbitRuntime::agent_corpus_for_activity(self, run_id, activity_name)
            .map_err(DispatchError::CliInvocationPermanent)
    }

    fn tag_instructions_for_activity(
        &self,
        run_id: &str,
        activity_name: &str,
        input: &Value,
    ) -> Result<Vec<TagInstruction>, DispatchError> {
        agent::tag_instructions_for_activity(self, run_id, activity_name, input)
    }

    fn explore_brief_for_activity(
        &self,
        run_id: &str,
        _activity_name: &str,
        input: &Value,
    ) -> Option<ExploreBriefAdmission> {
        task_context::explore_brief_for_agent_input(self, run_id, input)
    }

    fn experiment_overlays_for_activity(
        &self,
        run_id: &str,
        activity_name: &str,
        input: &Value,
    ) -> Result<Vec<ExperimentActivityOverlay>, DispatchError> {
        agent::experiment_overlays_for_activity(self, run_id, activity_name, input)
    }

    fn acquire_deferred_footprint(
        &self,
        run_id: &str,
        activity_name: &str,
    ) -> Result<(), DispatchError> {
        recovery::acquire_deferred_footprint(self, run_id, activity_name)
    }

    fn mid_run_steering_enabled(&self) -> bool {
        self.mid_run_steering().enabled
    }

    fn explore_delegation_available(&self) -> bool {
        self.explore_delegation_offered()
    }

    fn steering_gate(
        &self,
        run_id: &str,
        task_id: &str,
        candidate_source: Option<&str>,
    ) -> Result<SteeringGateReading, OrbitError> {
        self.steering_gate_reading(run_id, task_id, candidate_source)
    }

    fn admit_steering_round(
        &self,
        run_id: &str,
        task_id: &str,
        outstanding: &[CommentLogMark],
        candidate_source: Option<&str>,
    ) -> Result<SteeringRoundAdmission, OrbitError> {
        OrbitRuntime::admit_steering_round(self, run_id, task_id, outstanding, candidate_source)
    }

    fn run_deferral_probe(
        &self,
        probe: &DeferralProbe,
        provider: Option<&str>,
        fs_profile: Option<&str>,
        cwd: &Path,
    ) -> NamespaceProbe {
        orbit_engine::run_deferral_probe(self, probe, provider, fs_profile, cwd)
    }

    fn resolve_activity_tools(
        &self,
        task_ids: &[String],
        baseline_tools: &[String],
    ) -> Result<ResolvedActivityTools, DispatchError> {
        activity_tools::resolve_activity_tools(self, task_ids, baseline_tools)
    }

    fn resolve_activity_tool_denials(
        &self,
        task_ids: &[String],
        activity: &str,
        disallow_list: &[String],
    ) -> Result<ResolvedActivityTools, DispatchError> {
        activity_tools::resolve_activity_tool_denials(self, task_ids, activity, disallow_list)
    }

    fn checkpoint_step(
        &self,
        run_id: &str,
        step_index: u32,
        step_id: &str,
        output: &Value,
        compound_outputs: &BTreeMap<String, Value>,
        script_outputs: &BTreeMap<String, orbit_types::workflow::activity_job::ScriptOutputSet>,
    ) -> Result<(), DispatchError> {
        recovery::checkpoint_step(
            self,
            run_id,
            step_index,
            step_id,
            output,
            compound_outputs,
            script_outputs,
        )
    }

    fn observe_auto_task_step(
        &self,
        run_id: &str,
        request: &orbit_engine::AutoTaskStepRequest,
    ) -> Result<orbit_engine::AutoTaskObservation, DispatchError> {
        auto_task::observe_auto_task_step(self, run_id, request)
    }

    fn checkpoint_auto_task_step(
        &self,
        run_id: &str,
        step_id: &str,
        record: &orbit_types::workflow::AutoTaskStepRecord,
    ) -> Result<(), DispatchError> {
        auto_task::checkpoint_auto_task_step(self, run_id, step_id, record)
    }

    fn admit_auto_task_step(
        &self,
        run_id: &str,
        step_id: &str,
    ) -> Result<orbit_engine::AutoTaskAdmission, DispatchError> {
        auto_task::admit_auto_task_step(self, run_id, step_id)
    }

    fn checkpoint_parallel_branch(
        &self,
        run_id: &str,
        step_id: &str,
        branch_id: &str,
        output: &Value,
    ) -> Result<(), DispatchError> {
        auto_task::checkpoint_parallel_branch(self, run_id, step_id, branch_id, output)
    }

    fn park_job_run(
        &self,
        run_id: &str,
        park: &orbit_types::workflow::ParkedWait,
    ) -> Result<(), DispatchError> {
        auto_task::park_job_run(self, run_id, park)
    }

    fn checkpoint_step_diagnostic(
        &self,
        run_id: &str,
        step_id: &str,
        output: &Value,
    ) -> Result<(), DispatchError> {
        checkpoints::checkpoint_step_diagnostic(self, run_id, step_id, output)
    }

    fn checkpoint_failure_activity(
        &self,
        run_id: &str,
        activity_name: &str,
        failed_step_id: &str,
        output: &Value,
    ) -> Result<(), DispatchError> {
        checkpoints::checkpoint_failure_activity(
            self,
            run_id,
            activity_name,
            failed_step_id,
            output,
        )
    }

    fn final_recovery_log_tail(&self, run_id: &str) -> Result<Option<String>, OrbitError> {
        recovery::final_recovery_log_tail(self, run_id)
    }

    fn admit_final_recovery(
        &self,
        run_id: &str,
        request: &FinalRecoveryAdmissionRequest,
    ) -> Result<FinalRecoveryAdmission, OrbitError> {
        recovery::admit_final_recovery(self, run_id, request)
    }

    fn completion_deadline(
        &self,
        run_id: &str,
        pull_request: &str,
        proposed: u64,
    ) -> Result<u64, OrbitError> {
        checkpoints::completion_deadline(self, run_id, pull_request, proposed)
    }

    fn completion_policy_digest(
        &self,
        run_id: &str,
        task_ids: &[String],
    ) -> Result<Option<String>, OrbitError> {
        recovery::completion_policy_digest(self, run_id, task_ids)
    }

    fn completion_observation_admission(
        &self,
        run_id: &str,
        task_id: &str,
        step_id: &str,
    ) -> Result<CompletionObservationAdmission, OrbitError> {
        recovery::completion_observation_admission(self, run_id, task_id, step_id)
    }

    fn authenticate_completion_tail(
        &self,
        run_id: &str,
        task_id: &str,
        step_id: &str,
    ) -> Result<bool, OrbitError> {
        recovery::authenticate_completion_tail(self, run_id, task_id, step_id)
    }

    fn apply_final_recovery(
        &self,
        run_id: &str,
        application: &FinalRecoveryApplication,
    ) -> Result<FinalRecoveryApplied, OrbitError> {
        recovery::apply_final_recovery(self, run_id, application)
    }

    fn allocate_step_recovery_decision(
        &self,
        request: &StepRecoveryDecisionRequest,
    ) -> Result<Option<StepRecoveryDecisionSlot>, OrbitError> {
        recovery::allocate_step_recovery_decision(self, request)
    }

    fn read_step_recovery_decision(
        &self,
        slot: &StepRecoveryDecisionSlot,
    ) -> Result<StepRecoveryDecisionRead, OrbitError> {
        recovery::read_step_recovery_decision(self, slot)
    }

    fn source_repair_admission(
        &self,
        run_id: &str,
    ) -> Result<Option<orbit_types::workflow::SourceRepairAdmission>, OrbitError> {
        recovery::source_repair_admission(self, run_id)
    }

    fn owner_published_candidate(
        &self,
        task_id: &str,
        source_run_id: Option<&str>,
    ) -> Result<Option<orbit_types::workflow::SourceRepairBinding>, OrbitError> {
        recovery::owner_published_candidate(self, task_id, source_run_id)
    }
    fn owner_publication_binding(
        &self,
        run_id: &str,
    ) -> Result<Option<orbit_types::workflow::SourceRepairBinding>, OrbitError> {
        recovery::owner_publication_binding(self, run_id)
    }
    fn validate_source_repair(&self, run_id: &str) -> Result<(), OrbitError> {
        recovery::validate_source_repair(self, run_id)
    }

    fn admit_pr_auto_fix(
        &self,
        request: &orbit_engine::PrAutoFixRequest,
    ) -> Result<orbit_engine::PrAutoFixAdmission, OrbitError> {
        OrbitRuntime::admit_pr_auto_fix(self, request)
    }

    fn admit_handoff_base_update(
        &self,
        handoff_id: &str,
        request: &orbit_types::workflow::PrBaseUpdateReservation,
    ) -> Result<orbit_types::workflow::PrBaseUpdateAdmission, OrbitError> {
        OrbitRuntime::admit_handoff_base_update(self, handoff_id, request)
    }
    fn admit_pr_base_update(
        &self,
        task_ids: &[String],
        request: &orbit_types::workflow::PrBaseUpdateReservation,
    ) -> Result<orbit_types::workflow::PrBaseUpdateAdmission, OrbitError> {
        OrbitRuntime::admit_pr_base_update(self, task_ids, request)
    }

    fn admit_ci_rerun(
        &self,
        task_ids: &[String],
        handoff_id: Option<&str>,
        plan: &orbit_types::workflow::CiRerunPlan,
    ) -> Result<orbit_types::workflow::CiRerunAdmission, OrbitError> {
        OrbitRuntime::admit_ci_rerun(self, task_ids, handoff_id, plan)
    }
    fn admit_pr_infrastructure(
        &self,
        execution: &str,
        task_ids: &[String],
        evidence: &orbit_types::workflow::PrInfrastructureEvidence,
    ) -> Result<orbit_types::workflow::PrInfrastructureAdmission, OrbitError> {
        OrbitRuntime::admit_pr_infrastructure(self, execution, task_ids, evidence)
    }

    fn begin_rebase_recovery_attempt(
        &self,
        run_id: &str,
        step_id: &str,
        scope: &RebaseRecoveryAttemptScope,
    ) -> Result<u64, DispatchError> {
        checkpoints::begin_rebase_recovery_attempt(self, run_id, step_id, scope)
    }

    fn checkpoint_rebase_recovery(
        &self,
        run_id: &str,
        step_id: &str,
        output: &Value,
    ) -> Result<(), DispatchError> {
        checkpoints::checkpoint_rebase_recovery(self, run_id, step_id, output)
    }

    fn verify_rebase_recovery(
        &self,
        run_id: &str,
        step_id: &str,
        checkpoint: &Value,
    ) -> Result<bool, OrbitError> {
        checkpoints::verify_rebase_recovery(self, run_id, step_id, checkpoint)
    }

    fn rebase_recovery_attempts(
        &self,
        run_id: &str,
        step_id: &str,
    ) -> Result<Vec<RebaseRecoveryAttemptScope>, OrbitError> {
        checkpoints::rebase_recovery_attempts(self, run_id, step_id)
    }

    fn tool_context_for_activity(
        &self,
        run_id: Option<&str>,
        fs_profile: Option<&str>,
        fs_audit: Option<Arc<dyn FsAuditLogger>>,
        proc_allowed_programs: Option<&[String]>,
    ) -> ToolContext {
        activity_tools::tool_context_for_activity(
            self,
            run_id,
            fs_profile,
            fs_audit,
            proc_allowed_programs,
        )
    }

    fn begin_agent_execution(
        &self,
        run_id: &str,
        activity_id: &str,
        provider: &str,
        model: Option<&str>,
        identity: &orbit_types::telemetry::AgentExecutionIdentity,
    ) -> Result<Option<i64>, OrbitError> {
        Ok(Some(self.sqlite_store()?.begin_agent_execution(
            self.stores().jobs().workspace_id(),
            run_id,
            activity_id,
            provider,
            model,
            identity,
        )?))
    }

    fn persist_invocation_trace(
        &self,
        job_run_id: &str,
        activity_id: &str,
        input: &Value,
        invocation: &orbit_engine::DispatchInvocationTrace,
    ) -> Result<(), DispatchError> {
        invocation::persist_invocation_trace(self, job_run_id, activity_id, input, invocation)
    }

    fn agent_crew_config_for_input(
        &self,
        input: &serde_json::Value,
    ) -> Result<Option<CrewConfig>, DispatchError> {
        crew::agent_crew_config_for_input(self, input)
    }
}
