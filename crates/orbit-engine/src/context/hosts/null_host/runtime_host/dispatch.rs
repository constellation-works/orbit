//! The [`Dispatch`] answers of every [`NullRuntimeHost`].

use orbit_common::OrbitError;
use orbit_tools::ToolContext;
use orbit_types::workflow::{
    DeferralProbe, ExperimentActivityOverlay, NamespaceProbe, TagInstruction,
};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

use crate::context::dispatch_contract::{
    DispatchError, ResolvedCliExecutor, ResolvedSandbox, ResolvedShellExecutor,
};

use crate::context::hosts::{SteeringGateReading, SteeringRoundAdmission};

use crate::context::hosts::{Dispatch, NullRuntimeHost};

impl<T: NullRuntimeHost> Dispatch for T {
    fn mid_run_steering_enabled(&self) -> bool {
        NullRuntimeHost::mid_run_steering_enabled(self)
    }

    fn record_resident_continuity_rebuilt(&self, run_id: &str) -> Result<(), OrbitError> {
        NullRuntimeHost::record_resident_continuity_rebuilt(self, run_id)
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

    fn run_script_step(
        &self,
        request: &crate::context::ScriptStepRequest,
    ) -> crate::context::ScriptStepRun {
        NullRuntimeHost::run_script_step(self, request)
    }

    fn verify_script_output_set(
        &self,
        consumer_run_id: &str,
        set: &orbit_types::workflow::activity_job::ScriptOutputSet,
    ) -> Result<(), String> {
        NullRuntimeHost::verify_script_output_set(self, consumer_run_id, set)
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

    fn task_context_for_agent_input(
        &self,
        run_id: &str,
        step_id: &str,
        input: &Value,
    ) -> Result<Option<Value>, DispatchError> {
        NullRuntimeHost::task_context_for_agent_input(self, run_id, step_id, input)
    }

    fn agent_corpus_for_activity(
        &self,
        run_id: &str,
        activity_name: &str,
    ) -> Result<Option<orbit_types::workflow::AgentCorpusInjection>, DispatchError> {
        NullRuntimeHost::agent_corpus_for_activity(self, run_id, activity_name)
    }

    fn tag_instructions_for_activity(
        &self,
        run_id: &str,
        activity_name: &str,
        input: &Value,
    ) -> Result<Vec<TagInstruction>, DispatchError> {
        NullRuntimeHost::tag_instructions_for_activity(self, run_id, activity_name, input)
    }

    fn notices_for_agent_input(
        &self,
        run_id: &str,
        activity_name: &str,
        input: &Value,
    ) -> Option<orbit_types::workflow::FleetNoticeBrief> {
        NullRuntimeHost::notices_for_agent_input(self, run_id, activity_name, input)
    }

    fn experiment_overlays_for_activity(
        &self,
        run_id: &str,
        activity_name: &str,
        input: &Value,
        model: Option<&str>,
    ) -> Result<Vec<ExperimentActivityOverlay>, DispatchError> {
        NullRuntimeHost::experiment_overlays_for_activity(self, run_id, activity_name, input, model)
    }

    fn acquire_deferred_footprint(
        &self,
        run_id: &str,
        activity_name: &str,
    ) -> Result<(), DispatchError> {
        NullRuntimeHost::acquire_deferred_footprint(self, run_id, activity_name)
    }

    fn steering_gate(
        &self,
        run_id: &str,
        task_id: &str,
        candidate_source: Option<&str>,
    ) -> Result<SteeringGateReading, OrbitError> {
        NullRuntimeHost::steering_gate(self, run_id, task_id, candidate_source)
    }

    fn admit_steering_round(
        &self,
        run_id: &str,
        task_id: &str,
        outstanding: &[orbit_types::workflow::CommentLogMark],
        candidate_source: Option<&str>,
    ) -> Result<SteeringRoundAdmission, OrbitError> {
        NullRuntimeHost::admit_steering_round(self, run_id, task_id, outstanding, candidate_source)
    }
}
