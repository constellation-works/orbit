//! [`Recovery`] for [`EngineHost`]: each method delegates to a sibling
//! helper grouped by concern, or adapts a type on the way through.

use std::collections::BTreeMap;
use std::sync::Arc;

use orbit_common::OrbitError;
use orbit_engine::{
    CompletionObservationAdmission, CrewConfig, DispatchError, FinalRecoveryAdmission,
    FinalRecoveryAdmissionRequest, FinalRecoveryApplication, FinalRecoveryApplied,
    RebaseRecoveryAttemptScope, Recovery, ResolvedActivityTools,
};
use orbit_tools::{FsAuditLogger, ToolContext};
use serde_json::Value;

use crate::adapter::engine_host::runtime_host::{
    activity_tools, auto_task, checkpoints, crew, invocation, recovery, recovery_evidence,
};

use super::EngineHost;

impl Recovery for EngineHost<'_> {
    fn persist_final_recovery_evidence(
        &self,
        run_id: &str,
        task_id: &str,
        workspace: &std::path::Path,
        evidence: &Value,
    ) -> Result<(), OrbitError> {
        recovery_evidence::persist_final_recovery_evidence(
            self.runtime,
            run_id,
            task_id,
            workspace,
            evidence,
        )
    }

    fn resolve_activity_tools(
        &self,
        task_ids: &[String],
        baseline_tools: &[String],
    ) -> Result<ResolvedActivityTools, DispatchError> {
        activity_tools::resolve_activity_tools(self.runtime, task_ids, baseline_tools)
    }

    fn resolve_activity_tool_denials(
        &self,
        run_id: Option<&str>,
        task_ids: &[String],
        activity: &str,
        disallow_list: &[String],
    ) -> Result<ResolvedActivityTools, DispatchError> {
        activity_tools::resolve_activity_tool_denials(
            self.runtime,
            run_id,
            task_ids,
            activity,
            disallow_list,
        )
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
        recovery::checkpoint_step_outputs(
            self.runtime,
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
        recovery::checkpoint_skipped_step(self.runtime, run_id, step_index, step_id)
    }

    fn observe_auto_task_step(
        &self,
        run_id: &str,
        request: &orbit_engine::AutoTaskStepRequest,
    ) -> Result<orbit_engine::AutoTaskObservation, DispatchError> {
        auto_task::observe_auto_task_step(self.runtime, run_id, request)
    }

    fn checkpoint_auto_task_step(
        &self,
        run_id: &str,
        step_id: &str,
        record: &orbit_types::workflow::AutoTaskStepRecord,
    ) -> Result<(), DispatchError> {
        auto_task::checkpoint_auto_task_step(self.runtime, run_id, step_id, record)
    }

    fn admit_auto_task_step(
        &self,
        run_id: &str,
        step_id: &str,
    ) -> Result<orbit_engine::AutoTaskAdmission, DispatchError> {
        auto_task::admit_auto_task_step(self.runtime, run_id, step_id)
    }

    fn checkpoint_parallel_branch(
        &self,
        run_id: &str,
        step_id: &str,
        branch_id: &str,
        output: &Value,
    ) -> Result<(), DispatchError> {
        auto_task::checkpoint_parallel_branch(self.runtime, run_id, step_id, branch_id, output)
    }

    fn park_job_run(
        &self,
        run_id: &str,
        park: &orbit_types::workflow::ParkedWait,
    ) -> Result<(), DispatchError> {
        auto_task::park_job_run(self.runtime, run_id, park)
    }

    fn checkpoint_step_diagnostic(
        &self,
        run_id: &str,
        step_id: &str,
        output: &Value,
    ) -> Result<(), DispatchError> {
        checkpoints::checkpoint_step_diagnostic(self.runtime, run_id, step_id, output)
    }

    fn checkpoint_failure_activity(
        &self,
        run_id: &str,
        activity_name: &str,
        failed_step_id: &str,
        output: &Value,
    ) -> Result<(), DispatchError> {
        checkpoints::checkpoint_failure_activity(
            self.runtime,
            run_id,
            activity_name,
            failed_step_id,
            output,
        )
    }

    fn final_recovery_log_tail(&self, run_id: &str) -> Result<Option<String>, OrbitError> {
        recovery::final_recovery_log_tail(self.runtime, run_id)
    }

    fn admit_final_recovery(
        &self,
        run_id: &str,
        request: &FinalRecoveryAdmissionRequest,
    ) -> Result<FinalRecoveryAdmission, OrbitError> {
        recovery::admit_final_recovery(self.runtime, run_id, request)
    }

    fn completion_deadline(
        &self,
        run_id: &str,
        pull_request: &str,
        proposed: u64,
    ) -> Result<u64, OrbitError> {
        checkpoints::completion_deadline(self.runtime, run_id, pull_request, proposed)
    }

    fn completion_policy_digest(
        &self,
        run_id: &str,
        task_ids: &[String],
    ) -> Result<Option<String>, OrbitError> {
        recovery::completion_policy_digest(self.runtime, run_id, task_ids)
    }

    fn completion_observation_admission(
        &self,
        run_id: &str,
        task_id: &str,
        step_id: &str,
    ) -> Result<CompletionObservationAdmission, OrbitError> {
        recovery::completion_observation_admission(self.runtime, run_id, task_id, step_id)
    }

    fn authenticate_completion_tail(
        &self,
        run_id: &str,
        task_id: &str,
        step_id: &str,
    ) -> Result<bool, OrbitError> {
        recovery::authenticate_completion_tail(self.runtime, run_id, task_id, step_id)
    }

    fn apply_final_recovery(
        &self,
        run_id: &str,
        application: &FinalRecoveryApplication,
    ) -> Result<FinalRecoveryApplied, OrbitError> {
        recovery::apply_final_recovery(self.runtime, run_id, application)
    }

    fn begin_rebase_recovery_attempt(
        &self,
        run_id: &str,
        step_id: &str,
        scope: &RebaseRecoveryAttemptScope,
    ) -> Result<u64, DispatchError> {
        checkpoints::begin_rebase_recovery_attempt(self.runtime, run_id, step_id, scope)
    }

    fn checkpoint_rebase_recovery(
        &self,
        run_id: &str,
        step_id: &str,
        output: &Value,
    ) -> Result<(), DispatchError> {
        checkpoints::checkpoint_rebase_recovery(self.runtime, run_id, step_id, output)
    }

    fn verify_rebase_recovery(
        &self,
        run_id: &str,
        step_id: &str,
        checkpoint: &Value,
    ) -> Result<bool, OrbitError> {
        checkpoints::verify_rebase_recovery(self.runtime, run_id, step_id, checkpoint)
    }

    fn rebase_recovery_attempts(
        &self,
        run_id: &str,
        step_id: &str,
    ) -> Result<Vec<RebaseRecoveryAttemptScope>, OrbitError> {
        checkpoints::rebase_recovery_attempts(self.runtime, run_id, step_id)
    }

    fn tool_context_for_activity(
        &self,
        run_id: Option<&str>,
        fs_profile: Option<&str>,
        fs_audit: Option<Arc<dyn FsAuditLogger>>,
        proc_allowed_programs: Option<&[String]>,
    ) -> ToolContext {
        activity_tools::tool_context_for_activity(
            self.runtime,
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
        invocation::begin_agent_execution(
            self.runtime,
            run_id,
            activity_id,
            provider,
            model,
            identity,
        )
    }

    fn persist_invocation_trace(
        &self,
        job_run_id: &str,
        activity_id: &str,
        input: &Value,
        invocation: &orbit_engine::DispatchInvocationTrace,
    ) -> Result<(), DispatchError> {
        invocation::persist_invocation_trace(
            self.runtime,
            job_run_id,
            activity_id,
            input,
            invocation,
        )
    }

    fn agent_crew_config_for_input(
        &self,
        input: &serde_json::Value,
    ) -> Result<Option<CrewConfig>, DispatchError> {
        crew::agent_crew_config_for_input(self.runtime, input)
    }
}
