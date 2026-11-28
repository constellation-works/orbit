//! [`Dispatch`] for [`EngineHost`]: each method delegates to a sibling
//! helper grouped by concern, or adapts a type on the way through.

use crate::app::resident;
use std::collections::HashMap;
use std::path::Path;

use orbit_common::OrbitError;
use orbit_engine::{
    Dispatch, DispatchError, ResolvedCliExecutor, ResolvedSandbox, ResolvedShellExecutor,
    SteeringGateReading, SteeringRoundAdmission,
};
use orbit_tools::ToolContext;
use orbit_types::workflow::{
    CommentLogMark, DeferralProbe, ExperimentActivityOverlay, NamespaceProbe, PilotNotesResolution,
    TagInstruction,
};
use serde_json::Value;

use crate::OrbitRuntime;
use crate::adapter::engine_host::actions::{cli_executor, dispatch, sandbox, task_context};
use crate::adapter::engine_host::runtime_host::{agent, invocation, recovery};

use super::EngineHost;

impl Dispatch for EngineHost<'_> {
    fn run_script_step(
        &self,
        request: &orbit_engine::ScriptStepRequest,
    ) -> orbit_engine::ScriptStepRun {
        crate::application::job::script_step::run_script_step(self.runtime, request)
    }

    fn verify_script_output_set(
        &self,
        consumer_run_id: &str,
        set: &orbit_types::workflow::activity_job::ScriptOutputSet,
    ) -> Result<(), String> {
        crate::application::job::script_outputs::verify_script_output_set(
            self.runtime,
            consumer_run_id,
            set,
        )
    }

    fn run_deterministic(
        &self,
        action: &str,
        config: &Value,
        input: &Value,
        tool_context: ToolContext,
    ) -> Result<Value, DispatchError> {
        dispatch::run_deterministic(self.runtime, action, config, input, tool_context)
    }

    /// [ORB-10385] Report this binary's deterministic-action registry so job
    /// validation can reject a catalog asset naming an action we cannot
    /// dispatch, before the run admits a task or builds a worktree.
    fn has_deterministic_action(&self, action: &str) -> bool {
        dispatch::is_deterministic_action_registered(action)
    }

    fn resolve_cli_executor(&self, provider: &str) -> Result<ResolvedCliExecutor, DispatchError> {
        cli_executor::resolve_cli_executor(self.runtime, provider)
    }

    fn resolve_local_shell_executor(
        &self,
        executor: &str,
    ) -> Result<ResolvedShellExecutor, DispatchError> {
        cli_executor::resolve_local_shell_executor(self.runtime, executor)
    }

    fn provider_cli_config(&self, provider: &str) -> HashMap<String, String> {
        invocation::provider_cli_config(self.runtime, provider)
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
            self.runtime,
            self.runtime.env(),
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
        task_context::task_context_for_agent_input(self.runtime, run_id, step_id, input)
    }

    fn agent_corpus_for_activity(
        &self,
        run_id: &str,
        activity_name: &str,
    ) -> Result<Option<orbit_types::workflow::AgentCorpusInjection>, DispatchError> {
        crate::app::agent::agent_corpus_for_activity(self.runtime, run_id, activity_name)
            .map_err(DispatchError::CliInvocationPermanent)
    }

    fn tag_instructions_for_activity(
        &self,
        run_id: &str,
        activity_name: &str,
        input: &Value,
    ) -> Result<Vec<TagInstruction>, DispatchError> {
        agent::tag_instructions_for_activity(self.runtime, run_id, activity_name, input)
    }

    fn pilot_notes_for_activity(
        &self,
        _run_id: &str,
        activity_name: &str,
        input: &Value,
    ) -> Option<PilotNotesResolution> {
        agent::pilot_notes_for_activity(self.runtime, activity_name, input)
    }

    fn notices_for_agent_input(
        &self,
        run_id: &str,
        activity_name: &str,
        input: &Value,
    ) -> Option<orbit_types::workflow::FleetNoticeBrief> {
        task_context::notices_for_agent_input(self.runtime, run_id, activity_name, input)
    }

    fn experiment_overlays_for_activity(
        &self,
        run_id: &str,
        activity_name: &str,
        input: &Value,
        model: Option<&str>,
    ) -> Result<Vec<ExperimentActivityOverlay>, DispatchError> {
        agent::experiment_overlays_for_activity(self.runtime, run_id, activity_name, input, model)
    }

    fn acquire_deferred_footprint(
        &self,
        run_id: &str,
        activity_name: &str,
    ) -> Result<(), DispatchError> {
        recovery::acquire_deferred_footprint(self.runtime, run_id, activity_name)
    }

    fn mid_run_steering_enabled(&self) -> bool {
        self.runtime.mid_run_steering().enabled
    }

    fn record_resident_continuity_rebuilt(&self, run_id: &str) -> Result<(), OrbitError> {
        resident::record_resident_continuity_rebuilt_for_run(self.runtime, run_id)
    }

    fn steering_gate(
        &self,
        run_id: &str,
        task_id: &str,
        candidate_source: Option<&str>,
    ) -> Result<SteeringGateReading, OrbitError> {
        self.runtime
            .steering_gate_reading(run_id, task_id, candidate_source)
    }

    fn admit_steering_round(
        &self,
        run_id: &str,
        task_id: &str,
        outstanding: &[CommentLogMark],
        candidate_source: Option<&str>,
    ) -> Result<SteeringRoundAdmission, OrbitError> {
        OrbitRuntime::admit_steering_round(
            self.runtime,
            run_id,
            task_id,
            outstanding,
            candidate_source,
        )
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
}
