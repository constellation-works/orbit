use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use orbit_common::{NotFoundKind, OrbitError};
use orbit_engine::{
    CrewConfig, DispatchError, ResolvedActivityTools, ResolvedCliExecutor, ResolvedSandbox,
    ResolvedShellExecutor, RuntimeHost, TaskActivityUpdate, TaskAutomationUpdate, V2AuditWriter,
};
use orbit_store::contracts::{
    InvocationQuery, InvocationRecord, JobRunStepParams, TaskReservationReleaseReason,
};
use orbit_tools::{FsAuditLogger, ToolContext};
use orbit_types::identity::AgentModelPair;
use orbit_types::policy::Role;
use orbit_types::record::OrbitEvent;
use orbit_types::task::{
    ExternalRef, Task, TaskComment, TaskHistoryEntry, TaskPriority, TaskStatus,
};
use orbit_types::telemetry::InvocationTrace;
use orbit_types::workflow::{ActivityV2, JobRun, JobRunStartOutcome, JobRunState};
use serde_json::Value;

use crate::OrbitRuntime;
use crate::adapter::engine_host::v2_host::{cli_executor, dispatch, sandbox, task_context};
use crate::runtime::engine::paths::{codex_workspace_write_writable_dirs, current_repo_root};
use crate::runtime::recovery_authority::RecoveryAuthority;

use super::task_automation::apply_locked_task_automation_update;
use super::{activity_tools, checkpoints, crew, invocation};

impl RuntimeHost for OrbitRuntime {
    fn register_worker_pid_namespace(&self, pid: u32) -> Result<(), OrbitError> {
        #[cfg(target_os = "linux")]
        if let Some(binding) = self.worker_invocation() {
            RecoveryAuthority::open(&self.global_root())?.bind_worker_namespace(pid, binding)?;
        }
        #[cfg(not(target_os = "linux"))]
        let _ = pid;
        Ok(())
    }

    fn register_worker_process(&self, pid: u32) -> Result<(), OrbitError> {
        OrbitRuntime::register_worker_process(self, pid)
    }

    fn worker_invocation(&self) -> Option<orbit_types::tool::WorkerInvocation> {
        OrbitRuntime::worker_invocation(self).cloned()
    }
    fn record_direct_landing_intent(
        &self,
        request: &orbit_types::workflow::automation::DirectLandingRequest,
    ) -> Result<(), OrbitError> {
        crate::application::automation::record_direct_landing_intent(self, request)
    }

    fn insert_job_run(
        &self,
        job_id: &str,
        attempt: u32,
        scheduled_at: DateTime<Utc>,
        input: Option<serde_json::Value>,
        retry_source_run_id: Option<String>,
    ) -> Result<JobRun, OrbitError> {
        self.stores().jobs().insert_job_run(
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
        self.stores()
            .jobs()
            .mark_job_run_running(run_id, started_at, pid)
    }

    fn complete_job_run_step(
        &self,
        run_id: &str,
        params: &JobRunStepParams,
    ) -> Result<bool, OrbitError> {
        self.stores().jobs().complete_job_run_step(run_id, params)
    }

    fn finalize_job_run(
        &self,
        run_id: &str,
        state: JobRunState,
        finished_at: DateTime<Utc>,
        duration_ms: Option<u64>,
    ) -> Result<bool, OrbitError> {
        self.finalize_job_run_with_reservation_cleanup(
            run_id,
            state,
            finished_at,
            duration_ms,
            TaskReservationReleaseReason::RunTerminal,
        )
    }

    fn get_job_run(&self, run_id: &str) -> Result<Option<JobRun>, OrbitError> {
        match self.show_job_run(run_id) {
            Ok(run) => Ok(Some(run)),
            Err(OrbitError::NotFound {
                kind: NotFoundKind::JobRun,
                ..
            }) => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn read_run_state(
        &self,
        run_id: &str,
    ) -> Result<Option<orbit_types::workflow::PipelineState>, OrbitError> {
        self.stores().jobs().read_run_state(run_id)
    }

    fn write_run_state(
        &self,
        run_id: &str,
        state: &orbit_types::workflow::PipelineState,
    ) -> Result<(), OrbitError> {
        self.stores().jobs().write_run_state(run_id, state)
    }

    fn get_task(&self, task_id: &str) -> Result<Task, OrbitError> {
        OrbitRuntime::get_task(self, task_id)
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
        if let Some(binding) = self.worker_invocation() {
            if task_id != binding.task_id {
                return Err(OrbitError::PolicyDenied(format!(
                    "worker task binding mismatch: this leaf is bound to '{}', {workflow} \
                     requested '{task_id}'",
                    binding.task_id
                )));
            }
            let task: Task = self.read_owner(task_id, "task")?;
            if task.status != TaskStatus::InProgress {
                return Err(OrbitError::PolicyDenied(format!(
                    "claimed task '{task_id}' is '{}' on the owner; only an admitted \
                     in-progress claim may build a worktree",
                    task.status
                )));
            }
            return Ok(task);
        }
        OrbitRuntime::admit_task_for_workflow_as_system(self, task_id, workflow)
    }

    fn update_task_from_activity(
        &self,
        task_id: &str,
        update: TaskActivityUpdate,
    ) -> Result<Task, OrbitError> {
        if self.worker_invocation().is_some() {
            self.route_worker_tool("orbit.task.update", serde_json::json!({
                "id": task_id,
                "_worker_update": orbit_store::contracts::ClaimWorkerUpdate {
                    status: Some(update.status), expected_status: Some(update.expected_status),
                    status_note: update.note,
                    evidence: orbit_store::contracts::ClaimEvidence {summary: update.execution_summary, comment: update.comment, artifacts: vec![]},
                    ..Default::default()
                }
            }), Default::default())?;
            return self.get_task(task_id);
        }
        OrbitRuntime::update_task_from_activity(self, task_id, update)
    }

    fn validate_step_recovery_mutation(
        &self,
        run_id: &str,
        step_id: &str,
        task_ids: &[String],
        workspace_path: &std::path::Path,
    ) -> Result<(), OrbitError> {
        OrbitRuntime::validate_step_recovery_mutation(
            self,
            run_id,
            step_id,
            task_ids,
            workspace_path,
        )
    }

    fn record_review_landing(
        &self,
        request: &orbit_engine::ReviewLandingRequest,
    ) -> Result<(), OrbitError> {
        crate::application::review::record_review_landing(self, request)
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

    fn claim_execution_context(&self) -> Result<orbit_engine::ClaimExecutionContext, OrbitError> {
        let leaf = self.current_claimed_leaf()?;
        let ship = &leaf.admission.request.ship;
        Ok(orbit_engine::ClaimExecutionContext {
            workspace_id: leaf.binding.owner_workspace_id.clone(),
            task_id: leaf.claim.task_id.clone(),
            claim_id: leaf.claim.claim_id.clone(),
            machine_id: leaf.claim.executed_on.machine_id.clone(),
            run_id: leaf.binding.bound_run_id.clone(),
            ship_mode: ship.mode.clone(),
            base_branch: ship.base_branch.clone(),
            landing_branch: ship.landing_branch.clone(),
            // The owner re-derives its own requirements when it accepts the
            // handoff, so this copy only decides what the executor runs. A
            // follower reading a different list produces evidence the owner
            // refuses, which is the fail-closed direction.
            required_commands: self.workflow_required_validation_commands().to_vec(),
        })
    }

    fn attach_claim_validation_log(&self, path: &str, content: Vec<u8>) -> Result<(), OrbitError> {
        let leaf = self.current_claimed_leaf()?;
        orbit_types::task::validate_relative_artifact_path(path)?;
        // The same preloaded payload the spoke connector sends: bytes are read
        // on the executor and cross the coordination seam path-free, so the
        // evidence the owner later re-reads by digest lives in the owner's
        // store rather than on the executor's disk.
        self.route_worker_tool(
            "orbit.task.artifact.put",
            serde_json::json!({
                "id": leaf.claim.task_id,
                "artifacts": [{
                    "path": path,
                    "content": content,
                    "media_type": "application/json",
                }],
            }),
            Default::default(),
        )
        .map(|_| ())
    }

    fn record_claim_handoff(
        &self,
        handoff: &orbit_types::workflow::handoff::TaskHandoff,
    ) -> Result<(), OrbitError> {
        let leaf = self.current_claimed_leaf()?;
        if handoff.task_id != leaf.claim.task_id
            || handoff.claim_id != leaf.claim.claim_id
            || handoff.run_id != leaf.binding.bound_run_id
            || handoff.machine_id != leaf.claim.executed_on.machine_id
            || handoff.workspace_id != leaf.binding.owner_workspace_id
        {
            return Err(OrbitError::PolicyDenied(
                "handoff identity does not match this claim's trusted binding".into(),
            ));
        }
        // Durable before any owner call: a disconnect here leaves exactly one
        // immutable settlement for the drain to retry idempotently.
        self.stores().jobs().mutate_local_pull(
            &leaf.admission.destination,
            &leaf.admission.request.request_id,
            &orbit_store::contracts::LocalPullMutation::Settle(Box::new(
                orbit_store::contracts::ClaimMutation::AcceptHandoff(handoff.clone()),
            )),
        )?;
        Ok(())
    }

    fn apply_task_automation_update(
        &self,
        task_id: &str,
        update: TaskAutomationUpdate,
    ) -> Result<(), OrbitError> {
        if let Some(binding) = self.worker_invocation() {
            if update
                .job_run_id
                .as_ref()
                .is_some_and(|run| run != &binding.bound_run_id)
            {
                return Err(OrbitError::PolicyDenied(
                    "worker run binding mismatch".into(),
                ));
            }
            let comment = (!update.append_comments.is_empty()).then(|| {
                update
                    .append_comments
                    .iter()
                    .map(|comment| comment.message.as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            });
            self.route_worker_tool("orbit.task.update", serde_json::json!({
                "id": task_id,
                "_worker_update": orbit_store::contracts::ClaimWorkerUpdate {
                    status: update.status, plan: update.plan, context_files: update.context_files,
                    external_refs: update.external_refs, status_note: update.status_note,
                    evidence: orbit_store::contracts::ClaimEvidence {summary: update.execution_summary, comment, artifacts: vec![]},
                    ..Default::default()
                }
            }), Default::default())?;
            return Ok(());
        }
        apply_locked_task_automation_update(self, task_id, update)
    }

    fn agent_provider_config(&self) -> std::collections::HashMap<String, String> {
        let mut config = std::collections::HashMap::new();
        let policy = self.codex_execution_policy();
        config.insert("sandbox".to_string(), policy.sandbox().to_string());
        if let Some(approval) = policy.approval_policy() {
            config.insert("approval_policy".to_string(), approval.to_string());
        }
        if policy.sandbox() == "workspace-write" {
            config.insert(
                "writable_dirs_json".to_string(),
                serde_json::to_string(&codex_workspace_write_writable_dirs(self.context.paths()))
                    .unwrap_or_else(|_| "[]".to_string()),
            );
        }
        config
    }

    fn agent_subprocess_environment(&self, required_env_vars: &[&str]) -> Vec<(String, String)> {
        self.execution_env_policy()
            .agent_subprocess_env(required_env_vars)
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

    fn missing_required_environment_vars(&self, required_env_vars: &[&str]) -> Vec<String> {
        self.execution_env_policy()
            .missing_required(required_env_vars)
    }

    fn record_event(&self, event: OrbitEvent) -> Result<(), OrbitError> {
        OrbitRuntime::record_event(self, event)
    }

    fn repo_root(&self) -> Result<String, OrbitError> {
        current_repo_root(self)
    }

    fn list_job_runs_for_gc(&self) -> Result<Vec<JobRun>, OrbitError> {
        self.list_job_runs_for_worktree_gc()
    }

    fn data_root(&self) -> &std::path::Path {
        self.context.data_root()
    }

    fn cancel_job_run(&self, run_id: &str) -> Result<(), OrbitError> {
        OrbitRuntime::cancel_job_run(self, run_id).map(|_| ())
    }

    fn resolved_agent_model_pair(&self, agent_cli: &str) -> Option<AgentModelPair> {
        self.configured_agent_model_pair(agent_cli)
    }

    fn canonical_model_name(&self, agent_cli: &str, model: Option<&str>) -> Option<String> {
        let _ = agent_cli;
        model
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    }

    fn invocation_records(
        &self,
        query: InvocationQuery,
    ) -> Result<Vec<InvocationRecord>, OrbitError> {
        OrbitRuntime::invocation_records(self, query)
    }

    fn activity_implementer_identity(
        &self,
        input: &Value,
    ) -> Result<(Option<String>, Option<String>), OrbitError> {
        self.implementer_identity_for_activity_input(input)
    }

    fn resolved_crew_model(&self, run_id: &str) -> Result<Option<String>, OrbitError> {
        Ok(self
            .get_job_run_backend(run_id)?
            .and_then(|run| run.crew_model)
            .and_then(|model| {
                let model = model.trim();
                (!model.is_empty()).then(|| model.to_string())
            }))
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

    fn v2_runtime_host(&self) -> Result<&dyn RuntimeHost, OrbitError> {
        Ok(self)
    }

    fn v2_activity(&self, name: &str) -> Result<ActivityV2, OrbitError> {
        self.v2_activity_catalog()
            .map_err(|error| {
                OrbitError::InvalidInput(format!("build v2 activity catalog: {error}"))
            })?
            .get(name)
            .cloned()
            .ok_or_else(|| OrbitError::InvalidInput(format!("v2 activity '{name}' not found")))
    }

    fn v2_audit_writer(&self, run_id: &str) -> Result<Arc<V2AuditWriter>, OrbitError> {
        V2AuditWriter::with_disk_sinks(
            &self.paths().audit_dir,
            self.v2_audit_store()?,
            self.workspace_id()?,
            run_id,
            "system",
            Some(self.paths().repo_root.as_path()),
        )
        .map_err(|error| OrbitError::Execution(format!("v2 audit sinks: {error}")))
    }

    fn maybe_create_failure_task(
        &self,
        _job_id: &str,
        _run_id: &str,
        _error_code: &str,
        _error_message: &str,
        _agent: Option<&str>,
        _model: Option<&str>,
    ) -> Result<(), OrbitError> {
        Ok(())
    }

    fn scoring_enabled(&self) -> bool {
        self.context.scoring_enabled()
    }

    fn actor_model_identity(&self) -> Option<String> {
        matches!(self.actor().kind, crate::context::ActorKind::Agent)
            .then(|| self.actor_label().trim())
            .filter(|label| !label.is_empty())
            .map(ToOwned::to_owned)
    }

    fn pr_config(&self) -> orbit_engine::PrConfig {
        OrbitRuntime::pr_config(self).clone()
    }

    fn scoreboard_dir(&self) -> &std::path::Path {
        &self.context.paths().scoreboard_dir
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

    fn provider_cli_config(&self, _provider: &str) -> HashMap<String, String> {
        RuntimeHost::agent_provider_config(self)
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

    fn task_context_for_agent_input(&self, input: &Value) -> Result<Option<Value>, DispatchError> {
        task_context::task_context_for_agent_input(self, input)
    }

    fn resolve_activity_tools(
        &self,
        task_ids: &[String],
        baseline_tools: &[String],
    ) -> Result<ResolvedActivityTools, DispatchError> {
        activity_tools::resolve_activity_tools(self, task_ids, baseline_tools)
    }

    fn checkpoint_step(
        &self,
        run_id: &str,
        step_index: u32,
        step_id: &str,
        output: &Value,
    ) -> Result<(), DispatchError> {
        checkpoints::checkpoint_step(self, run_id, step_index, step_id, output)
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

    fn persist_invocation_trace(
        &self,
        job_run_id: &str,
        activity_id: &str,
        provider: &str,
        model: Option<&str>,
        input: &Value,
        trace: &InvocationTrace,
    ) -> Result<(), DispatchError> {
        invocation::persist_invocation_trace(
            self,
            job_run_id,
            activity_id,
            provider,
            model,
            input,
            trace,
        )
    }

    fn system_crew_for_dispatch(&self) -> Option<String> {
        Some(self.context.settings().system_crew().to_string())
    }

    fn agent_crew_config_for_input(
        &self,
        input: &serde_json::Value,
    ) -> Result<Option<CrewConfig>, DispatchError> {
        crew::agent_crew_config_for_input(self, input)
    }
}
