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
    InvocationInsertParams, InvocationQuery, InvocationRecord, JobRunStepParams,
    TaskReservationReleaseReason,
};
use orbit_tools::{FsAuditLogger, ReservationOwnerContext, ToolContext};
use orbit_types::identity::AgentModelPair;
use orbit_types::policy::{Role, UNRESTRICTED_FS_PROFILE};
use orbit_types::record::OrbitEvent;
use orbit_types::task::{
    ExternalRef, Task, TaskComment, TaskHistoryEntry, TaskPriority, TaskStatus,
    push_external_ref_if_missing,
};
use orbit_types::telemetry::InvocationTrace;
use orbit_types::tool::{ToolSessionContext, is_exact_canonical_tool_name};
use orbit_types::workflow::{ActivityV2, JobRun, JobRunStartOutcome, JobRunState};
use serde_json::Value;

use crate::OrbitRuntime;
use crate::adapter::engine_host::v2_host::{cli_executor, dispatch, sandbox, task_context};
use crate::adapter::tool_host::build_orbit_tool_host;
use crate::application::task::TaskRecordUpdateParams as StoreTaskUpdateParams;
use crate::application::task::{
    SYSTEM_ACTOR_LABEL, TaskAttributionInput, TaskUpdateParams, assemble_task_attribution,
};
use crate::runtime::engine::paths::{codex_workspace_write_writable_dirs, current_repo_root};
use crate::runtime::recovery_authority::RecoveryAuthority;

impl RuntimeHost for OrbitRuntime {
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

    fn admit_task_for_workflow(&self, task_id: &str, workflow: &str) -> Result<Task, OrbitError> {
        OrbitRuntime::admit_task_for_workflow_as_system(self, task_id, workflow)
    }

    fn update_task_from_activity(
        &self,
        task_id: &str,
        update: TaskActivityUpdate,
    ) -> Result<Task, OrbitError> {
        OrbitRuntime::update_task_from_activity(self, task_id, update)
    }

    fn authorize_step_recovery(
        &self,
        run_id: &str,
        step_id: &str,
    ) -> Result<orbit_engine::StepRecoveryAdmission, OrbitError> {
        OrbitRuntime::authorize_step_recovery(self, run_id, step_id)
    }

    fn settle_step_recovery(
        &self,
        run_id: &str,
        step_id: &str,
        elapsed_seconds: u64,
    ) -> Result<(), OrbitError> {
        OrbitRuntime::settle_step_recovery(self, run_id, step_id, elapsed_seconds)
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

    fn authorize_task_completion(
        &self,
        run_id: &str,
        task_ids: &[String],
    ) -> Result<(), OrbitError> {
        OrbitRuntime::authorize_task_completion(self, run_id, task_ids)
    }

    fn record_review_landing(
        &self,
        request: &orbit_engine::ReviewLandingRequest,
    ) -> Result<(), OrbitError> {
        crate::application::review::record_review_landing(self, request)
    }

    fn apply_task_automation_update(
        &self,
        task_id: &str,
        update: TaskAutomationUpdate,
    ) -> Result<(), OrbitError> {
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
        self.list_job_runs(crate::application::job::JobRunListParams::default())
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
        if task_ids.is_empty() {
            return Ok(ResolvedActivityTools {
                requested_tools: Vec::new(),
                effective_tools: baseline_tools.to_vec(),
            });
        }
        let mut requested_tools = std::collections::BTreeSet::new();
        for task_id in task_ids {
            let task = self.get_task(task_id).map_err(|error| {
                DispatchError::CliInvocationFailed(format!(
                    "load task `{task_id}` tool requirements: {error}"
                ))
            })?;
            for tool_name in task.required_tools {
                let reason = if tool_name.contains('*') {
                    Some("wildcard and prefix requirements are not allowed".to_string())
                } else if !is_exact_canonical_tool_name(&tool_name) {
                    Some("malformed canonical tool name".to_string())
                } else if !self.tool_registry().has(&tool_name) {
                    Some("unknown registered tool".to_string())
                } else if !self.tool_registry().is_active(&tool_name) {
                    Some("tool is not agent-facing".to_string())
                } else {
                    self.stores()
                        .tools()
                        .get_tool(&tool_name)
                        .map_err(|error| {
                            DispatchError::CliInvocationFailed(format!(
                                "read tool `{tool_name}` admission state for task `{task_id}`: {error}"
                            ))
                        })?
                        .filter(|tool| !tool.enabled)
                        .map(|_| "tool is inactive".to_string())
                };
                if let Some(reason) = reason {
                    return Err(DispatchError::RequiredToolAdmission {
                        task_id: task_id.clone(),
                        tool_name,
                        reason,
                    });
                }
                requested_tools.insert(tool_name);
            }
        }
        let requested_tools = requested_tools.into_iter().collect::<Vec<_>>();

        if requested_tools.is_empty() {
            return Ok(ResolvedActivityTools {
                requested_tools,
                effective_tools: baseline_tools.to_vec(),
            });
        }

        let mut seen = std::collections::BTreeSet::new();
        let mut effective_tools = Vec::with_capacity(baseline_tools.len() + requested_tools.len());
        for tool in baseline_tools.iter().chain(requested_tools.iter()) {
            if seen.insert(tool.clone()) {
                effective_tools.push(tool.clone());
            }
        }
        Ok(ResolvedActivityTools {
            requested_tools,
            effective_tools,
        })
    }

    /// [ORB-10002] Persist a per-step checkpoint into the run's
    /// `PipelineState` so an interrupted run can be resumed without
    /// re-executing completed steps. A missing run row (direct `execute_job`
    /// callers that never persisted a run) is a silent no-op — there is
    /// nothing durable to checkpoint into.
    fn checkpoint_step(
        &self,
        run_id: &str,
        step_index: u32,
        step_id: &str,
        output: &Value,
        pipeline_snapshot: &Value,
    ) -> Result<(), DispatchError> {
        // [ORB-11253] Read-modify-write in one transaction rather than a
        // separate read and write: an operator run control written into this
        // same state document between the two would otherwise be silently
        // discarded by the checkpoint that follows it.
        self.stores()
            .jobs()
            .update_run_state(run_id, &mut |_, state| {
                state.record_step(
                    step_index,
                    orbit_types::workflow::JobRunState::Success,
                    Some(output.clone()),
                    None,
                );
                state.sync_pipeline(pipeline_snapshot.clone());
                Ok(())
            })
            .map(|_| ())
            .map_err(|error| {
                DispatchError::JobExecution(format!(
                    "persist step checkpoint (run {run_id}, step {step_index} `{step_id}`): {error}"
                ))
            })
    }

    fn checkpoint_failure_activity(
        &self,
        run_id: &str,
        activity_name: &str,
        failed_step_id: &str,
        output: &Value,
    ) -> Result<(), DispatchError> {
        self.stores()
            .jobs()
            .update_run_state(run_id, &mut |_, state| {
                state.record_failure_activity(
                    activity_name.to_string(),
                    failed_step_id.to_string(),
                    output.clone(),
                );
                Ok(())
            })
            .map(|_| ())
            .map_err(|error| {
                DispatchError::JobExecution(format!(
                    "persist terminal failure activity checkpoint (run {run_id}, step \
                     `{failed_step_id}`, activity `{activity_name}`): {error}"
                ))
            })
    }

    /// Certify the host's completion first, then persist the advisory copy.
    ///
    /// The certificate lives outside every leaf write grant; the run-state
    /// entry that follows it is progress data a leaf can rewrite. Either half
    /// failing leaves the run without usable evidence rather than with
    /// unauthenticated evidence, so both orders are fail-closed.
    fn checkpoint_rebase_recovery(
        &self,
        run_id: &str,
        step_id: &str,
        output: &Value,
    ) -> Result<(), DispatchError> {
        RecoveryAuthority::open(&self.global_root())
            .and_then(|authority| authority.issue(run_id, step_id, output))
            .map_err(|error| {
                DispatchError::JobExecution(format!(
                    "certify rebase recovery (run {run_id}, step `{step_id}`): {error}"
                ))
            })?;

        self.stores()
            .jobs()
            .update_run_state(run_id, &mut |run_state, state| {
                if run_state != orbit_types::workflow::JobRunState::Running {
                    return Err(orbit_common::OrbitError::Execution(
                        "rebase recovery run is no longer running".to_string(),
                    ));
                }
                state
                    .rebase_recovery_checkpoints
                    .insert(step_id.to_string(), output.clone());
                state.updated_at = Utc::now();
                Ok(())
            })
            .and_then(|updated| {
                if matches!(updated, orbit_types::workflow::RunStateUpdate::Updated) {
                    Ok(())
                } else {
                    Err(orbit_common::OrbitError::Execution(
                        "rebase recovery has no durable run state".to_string(),
                    ))
                }
            })
            .map_err(|error| {
                DispatchError::JobExecution(format!(
                    "persist rebase recovery checkpoint (run {run_id}, step `{step_id}`): {error}"
                ))
            })
    }

    fn verify_rebase_recovery(
        &self,
        run_id: &str,
        step_id: &str,
        checkpoint: &Value,
    ) -> Result<bool, OrbitError> {
        RecoveryAuthority::open(&self.global_root())?.verify(run_id, step_id, checkpoint)
    }

    fn tool_context_for_activity(
        &self,
        run_id: Option<&str>,
        fs_profile: Option<&str>,
        fs_audit: Option<Arc<dyn FsAuditLogger>>,
        proc_allowed_programs: Option<&[String]>,
    ) -> ToolContext {
        let workspace_root = self
            .paths()
            .repo_root
            .canonicalize()
            .unwrap_or_else(|_| self.paths().repo_root.clone());

        // Every context built here belongs to a v2 activity, so `proc.spawn` is
        // always activity-scoped: a missing `proc_allowed_programs` denies every
        // program instead of degrading to allow-all. Asset load already refuses
        // an activity that grants `proc.spawn` without the key ([ORB-10959]);
        // this keeps the enforcement point fail-closed on its own.
        let proc_spawn_activity_scoped = true;
        let proc_allowed_programs = proc_allowed_programs
            .map(|programs| programs.to_vec())
            .unwrap_or_default();
        let proc_spawn_environment = Some(self.execution_env_policy().agent_subprocess_env(&[]));

        ToolContext {
            cwd: std::env::current_dir()
                .ok()
                .map(|cwd| cwd.to_string_lossy().into_owned()),
            workspace_root: Some(workspace_root),
            policy_engine: Some(Arc::new(self.policy_engine().clone())),
            fs_profile: Some(fs_profile.unwrap_or(UNRESTRICTED_FS_PROFILE).to_string()),
            fs_audit,
            proc_allowed_programs,
            proc_spawn_environment,
            proc_spawn_activity_scoped,
            reservation_owner: run_id.map(str::trim).filter(|value| !value.is_empty()).map(
                |owner_run_id| ReservationOwnerContext {
                    owner_run_id: owner_run_id.to_string(),
                    owner_metadata_json: Some(
                        serde_json::json!({
                            "source": "v2_activity",
                            "fs_profile": fs_profile.unwrap_or(UNRESTRICTED_FS_PROFILE),
                        })
                        .to_string(),
                    ),
                },
            ),
            orbit_host: Some(build_orbit_tool_host(
                self,
                None,
                run_id
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned),
                ToolSessionContext::default(),
            )),
            ..Default::default()
        }
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
        let (agent, model) = self.invocation_agent_model_identity(
            provider,
            model,
            trace.provider_model.as_deref(),
            job_run_id,
            activity_id,
        );
        self.insert_invocation_trace_record(&InvocationInsertParams {
            job_run_id: job_run_id.to_string(),
            activity_id: activity_id.to_string(),
            agent: agent.unwrap_or_else(|| provider.to_ascii_lowercase()),
            model,
            task_ids: task_context::associated_task_ids(input),
            trace: trace.clone(),
        })
        .map_err(|error| {
            DispatchError::JobExecution(format!("persist invocation trace: {error}"))
        })?;

        let existing = self
            .get_job_run_backend(job_run_id)
            .map_err(|error| {
                DispatchError::JobExecution(format!("read job run for knowledge metrics: {error}"))
            })?
            .and_then(|run| run.knowledge_metrics);
        if let Some(metrics) = crate::metrics::merge_invocation_trace(existing.as_ref(), trace) {
            self.stores()
                .jobs()
                .record_job_run_knowledge_metrics(job_run_id, metrics)
                .map_err(|error| {
                    DispatchError::JobExecution(format!(
                        "record job-run knowledge metrics: {error}"
                    ))
                })?;
        }

        Ok(())
    }

    fn system_crew_for_dispatch(&self) -> Option<String> {
        Some(self.context.settings().system_crew().to_string())
    }

    fn agent_crew_config_for_input(
        &self,
        input: &serde_json::Value,
    ) -> Result<Option<CrewConfig>, DispatchError> {
        let explicit = input
            .get("crew")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let config_key = input
            .get("crew_config_key")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let crew = match config_key {
            Some("workflow.system_crew") => self
                .resolve_crew_for_task(Some(self.context.settings().system_crew()), None)
                .map_err(|error| {
                    DispatchError::JobValidation(format!(
                        "activity crew configured by `workflow.system_crew` cannot be resolved or used: {error}"
                    ))
                })?,
            Some(other) => {
                return Err(DispatchError::JobValidation(format!(
                    "activity crew names unsupported configuration key `{other}`"
                )));
            }
            None => match explicit {
                Some(crew_name) => self
                    .resolve_crew_for_task(Some(crew_name), None)
                    .map_err(|error| {
                        DispatchError::JobValidation(format!(
                            "explicit activity crew `{crew_name}` cannot be resolved or used: {error}"
                        ))
                    })?,
                None => self.resolve_crew_for_run_input(input).map_err(|error| {
                    DispatchError::JobValidation(format!(
                        "run crew cannot be resolved or used for activity dispatch: {error}"
                    ))
                })?,
            },
        };
        // [ORB-11242] The last gate before a provider process is launched, and
        // the only one that sees the crew *after* alias and default resolution.
        // A system override, an explicit activity crew, and the run's own crew
        // all arrive here, so one check covers every route into a provider.
        let allowlist = self.crew_allowlist_from_input(input).map_err(|error| {
            DispatchError::JobValidation(format!(
                "run crew allowlist cannot be resolved for activity dispatch: {error}"
            ))
        })?;
        let origin = match config_key {
            Some(key) => format!("`{key}`"),
            None if explicit.is_some() => "an explicit activity crew".to_string(),
            None => "this run's crew".to_string(),
        };
        crate::runtime::engine::crew::enforce_crew_allowlist(allowlist.as_ref(), &crew, &origin)
            .map_err(|error| DispatchError::JobValidation(error.to_string()))?;
        Ok(Some(
            crate::runtime::engine::environment_host::typed_crew_config_from_assignment(
                &crew.assignment,
            ),
        ))
    }
}

/// Apply an automation update while holding the task write lock across the
/// whole read-modify-write.
///
/// ORB-11623: delivery automation previously read the task, derived
/// attribution and a replacement `external_refs` vector, then wrote through
/// `task_records().update` with no lock and `expected_status: None`. The
/// store locks each write, not the read that decided it, so an operator
/// transition or another ref writer in that gap was overwritten. The lock
/// is re-entrant per thread, so the store's own per-write locking still
/// holds underneath.
fn apply_locked_task_automation_update(
    runtime: &OrbitRuntime,
    task_id: &str,
    update: TaskAutomationUpdate,
) -> Result<(), OrbitError> {
    let mut update = Some(update);
    let mut updated: Option<Task> = None;
    runtime
        .stores()
        .tasks()
        .with_task_write_lock(task_id, &mut || {
            let update = update.take().ok_or_else(|| {
                OrbitError::Execution(
                    "task automation update body was invoked more than once".to_string(),
                )
            })?;
            updated = Some(apply_task_automation_update_under_lock(
                runtime, task_id, update,
            )?);
            Ok(())
        })?;
    let task = updated.ok_or_else(|| {
        OrbitError::Execution(
            "task automation update body did not run under the task lock".to_string(),
        )
    })?;
    if task.status == TaskStatus::Done {
        runtime.record_resolves_side_effects(&task)?;
    }
    Ok(())
}

fn apply_task_automation_update_under_lock(
    runtime: &OrbitRuntime,
    task_id: &str,
    update: TaskAutomationUpdate,
) -> Result<Task, OrbitError> {
    let existing_task = runtime.get_task(task_id)?;
    #[cfg(test)]
    runtime.invoke_after_locked_state_read(&existing_task);
    if update.status == Some(TaskStatus::InProgress)
        && crate::application::task::in_progress_transition_requires_plan(existing_task.status)
    {
        crate::application::task::ensure_task_has_execution_plan(
            task_id,
            existing_task.plan.as_str(),
        )?;
    }
    if update.status == Some(TaskStatus::Done) && existing_task.status != TaskStatus::Done {
        runtime.ensure_resolves_are_workspace_local(&existing_task)?;
    }
    let (agent, model) =
        crate::context::trusted_write_identity(update.agent.as_deref(), update.model.as_deref());
    let runtime_model_identity = <OrbitRuntime as RuntimeHost>::actor_model_identity(runtime);
    let attribution = assemble_task_attribution(
        &existing_task,
        TaskAttributionInput {
            default_actor_label: SYSTEM_ACTOR_LABEL,
            actor_override: Some(SYSTEM_ACTOR_LABEL),
            agent: agent.as_deref(),
            model: model.as_deref(),
            runtime_model_identity: runtime_model_identity.as_deref(),
            plan_changed: update.plan.is_some(),
            target_status: update.status,
            explicit_planned_by: None,
            explicit_implemented_by: None,
        },
    )?;
    runtime.with_mutation(|| {
        let external_refs = if update.external_refs.is_empty() {
            None
        } else {
            // Merge against the latest locked state. `external_refs` is a
            // wholesale replacement in the store, so a re-entrant writer that
            // landed a ref after the decision snapshot would otherwise be lost.
            let mut refs = runtime.get_task(task_id)?.external_refs;
            for external_ref in update.external_refs.clone() {
                push_external_ref_if_missing(&mut refs, external_ref);
            }
            Some(refs)
        };
        let task = runtime.stores().task_records().update(
            task_id,
            StoreTaskUpdateParams {
                actor: attribution.actor.clone(),
                planned_by: attribution.planned_by.clone(),
                implemented_by: attribution.implemented_by.clone(),
                external_refs,
                status_event: update.status_event.clone(),
                status_note: update.status_note.clone(),
                append_comments: update.append_comments.clone(),
                expected_status: Some(vec![existing_task.status]),
                ..StoreTaskUpdateParams::from(TaskUpdateParams {
                    execution_summary: update.execution_summary.clone(),
                    plan: update.plan.clone(),
                    context_files: update.context_files.clone(),
                    status: update.status,
                    job_run_id: update.job_run_id.clone().map(Some),
                    ..Default::default()
                })
            },
        )?;
        Ok((
            task.clone(),
            OrbitEvent::TaskUpdated {
                id: task_id.to_string(),
            },
        ))
    })
}
