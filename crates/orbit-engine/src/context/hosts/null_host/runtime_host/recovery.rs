//! The [`Recovery`] answers of every [`NullRuntimeHost`].

use orbit_common::OrbitError;
use orbit_tools::{FsAuditLogger, ToolContext};
use orbit_types::workflow::{AutoTaskStepRecord, ParkedWait};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::context::dispatch_contract::{DispatchError, DispatchInvocationTrace};

use crate::context::hosts::{AutoTaskAdmission, AutoTaskObservation, AutoTaskStepRequest};
use crate::context::hosts::{
    CompletionObservationAdmission, CrewConfig, FinalRecoveryAdmission,
    FinalRecoveryAdmissionRequest, FinalRecoveryApplication, FinalRecoveryApplied,
    RebaseRecoveryAttemptScope, ResolvedActivityTools,
};

use crate::context::hosts::{NullRuntimeHost, Recovery};

impl<T: NullRuntimeHost> Recovery for T {
    fn completion_deadline(
        &self,
        run_id: &str,
        pull_request: &str,
        proposed: u64,
    ) -> Result<u64, OrbitError> {
        NullRuntimeHost::completion_deadline(self, run_id, pull_request, proposed)
    }

    fn completion_policy_digest(
        &self,
        run_id: &str,
        task_ids: &[String],
    ) -> Result<Option<String>, OrbitError> {
        NullRuntimeHost::completion_policy_digest(self, run_id, task_ids)
    }

    fn authenticate_completion_tail(
        &self,
        run_id: &str,
        task_id: &str,
        step_id: &str,
    ) -> Result<bool, OrbitError> {
        NullRuntimeHost::authenticate_completion_tail(self, run_id, task_id, step_id)
    }

    fn resolve_activity_tool_denials(
        &self,
        run_id: Option<&str>,
        task_ids: &[String],
        activity: &str,
        disallow_list: &[String],
    ) -> Result<ResolvedActivityTools, DispatchError> {
        NullRuntimeHost::resolve_activity_tool_denials(
            self,
            run_id,
            task_ids,
            activity,
            disallow_list,
        )
    }

    fn resolve_activity_tools(
        &self,
        task_ids: &[String],
        baseline_tools: &[String],
    ) -> Result<ResolvedActivityTools, DispatchError> {
        NullRuntimeHost::resolve_activity_tools(self, task_ids, baseline_tools)
    }

    fn observe_auto_task_step(
        &self,
        run_id: &str,
        request: &AutoTaskStepRequest,
    ) -> Result<AutoTaskObservation, DispatchError> {
        NullRuntimeHost::observe_auto_task_step(self, run_id, request)
    }

    fn checkpoint_auto_task_step(
        &self,
        run_id: &str,
        step_id: &str,
        record: &AutoTaskStepRecord,
    ) -> Result<(), DispatchError> {
        NullRuntimeHost::checkpoint_auto_task_step(self, run_id, step_id, record)
    }

    fn admit_auto_task_step(
        &self,
        run_id: &str,
        step_id: &str,
    ) -> Result<AutoTaskAdmission, DispatchError> {
        NullRuntimeHost::admit_auto_task_step(self, run_id, step_id)
    }

    fn checkpoint_parallel_branch(
        &self,
        run_id: &str,
        step_id: &str,
        branch_id: &str,
        output: &Value,
    ) -> Result<(), DispatchError> {
        NullRuntimeHost::checkpoint_parallel_branch(self, run_id, step_id, branch_id, output)
    }

    fn park_job_run(&self, run_id: &str, park: &ParkedWait) -> Result<(), DispatchError> {
        NullRuntimeHost::park_job_run(self, run_id, park)
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
        NullRuntimeHost::checkpoint_step(
            self,
            run_id,
            step_index,
            step_id,
            output,
            compound_outputs,
            script_outputs,
        )
    }

    fn checkpoint_skipped_step(
        &self,
        run_id: &str,
        step_index: u32,
        step_id: &str,
    ) -> Result<(), DispatchError> {
        NullRuntimeHost::checkpoint_skipped_step(self, run_id, step_index, step_id)
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

    fn completion_observation_admission(
        &self,
        run_id: &str,
        task_id: &str,
        step_id: &str,
    ) -> Result<CompletionObservationAdmission, OrbitError> {
        NullRuntimeHost::completion_observation_admission(self, run_id, task_id, step_id)
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

    fn persist_final_recovery_evidence(
        &self,
        run_id: &str,
        task_id: &str,
        workspace: &std::path::Path,
        evidence: &Value,
    ) -> Result<(), OrbitError> {
        NullRuntimeHost::persist_final_recovery_evidence(self, run_id, task_id, workspace, evidence)
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

    fn begin_agent_execution(
        &self,
        run_id: &str,
        activity_id: &str,
        provider: &str,
        model: Option<&str>,
        identity: &orbit_types::telemetry::AgentExecutionIdentity,
    ) -> Result<Option<i64>, OrbitError> {
        NullRuntimeHost::begin_agent_execution(self, run_id, activity_id, provider, model, identity)
    }

    fn persist_invocation_trace(
        &self,
        job_run_id: &str,
        activity_id: &str,
        input: &Value,
        invocation: &DispatchInvocationTrace,
    ) -> Result<(), DispatchError> {
        NullRuntimeHost::persist_invocation_trace(self, job_run_id, activity_id, input, invocation)
    }

    fn agent_crew_config_for_input(
        &self,
        input: &Value,
    ) -> Result<Option<CrewConfig>, DispatchError> {
        NullRuntimeHost::agent_crew_config_for_input(self, input)
    }
}
