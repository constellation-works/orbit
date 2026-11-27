//! The [`HostEnvironment`] answers of every [`NullRuntimeHost`].

use std::collections::HashMap;

use orbit_common::OrbitError;
use orbit_store::contract::{InvocationQuery, InvocationRecord};
use orbit_tools::ToolContext;
use orbit_types::identity::AgentModelPair;
use orbit_types::record::OrbitEvent;
use orbit_types::telemetry::ProviderLimitObservation;
use orbit_types::workflow::JobRun;
use serde_json::Value;

use crate::context::RunSettings;
use crate::context::hosts::{HostEnvironment, NullRuntimeHost};
use crate::context::hosts::{ScratchGcReport, WorktreeGcTaskLookup};

impl<T: NullRuntimeHost> HostEnvironment for T {
    fn run_settings(&self) -> RunSettings {
        NullRuntimeHost::run_settings(self)
    }

    fn admit_owed_criteria(
        &self,
        task_id: &str,
        run_id: &str,
        owed: &[orbit_types::workflow::OwedCriterion],
    ) -> Result<(), OrbitError> {
        NullRuntimeHost::admit_owed_criteria(self, task_id, run_id, owed)
    }

    fn outstanding_owed_criteria(
        &self,
        task_id: &str,
        run_id: Option<&str>,
    ) -> Result<Vec<usize>, OrbitError> {
        NullRuntimeHost::outstanding_owed_criteria(self, task_id, run_id)
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

    fn record_validated_tree(
        &self,
        tree: &crate::context::ValidatedTree,
    ) -> Result<(), OrbitError> {
        NullRuntimeHost::record_validated_tree(self, tree)
    }

    fn validated_tree(
        &self,
        run_id: &str,
    ) -> Result<Option<crate::context::ValidatedTree>, OrbitError> {
        NullRuntimeHost::validated_tree(self, run_id)
    }

    fn agent_provider_config(&self) -> HashMap<String, String> {
        NullRuntimeHost::agent_provider_config(self)
    }

    fn agent_subprocess_environment(&self, required_env_vars: &[&str]) -> Vec<(String, String)> {
        NullRuntimeHost::agent_subprocess_environment(self, required_env_vars)
    }

    fn crew_agent_subprocess_environment(
        &self,
        crew_env: &[orbit_types::identity::CrewEnvVar],
        required_env_vars: &[&str],
    ) -> Vec<(String, String)> {
        NullRuntimeHost::crew_agent_subprocess_environment(self, crew_env, required_env_vars)
    }

    fn validation_subprocess_environment(&self) -> orbit_exec::ValidationEnvironment {
        NullRuntimeHost::validation_subprocess_environment(self)
    }

    fn refresh_persistence_after_cli_provider(&self) -> Result<(), OrbitError> {
        NullRuntimeHost::refresh_persistence_after_cli_provider(self)
    }

    fn record_event(&self, event: OrbitEvent) -> Result<(), OrbitError> {
        NullRuntimeHost::record_event(self, event)
    }

    fn list_job_runs_for_gc(&self) -> Result<Vec<JobRun>, OrbitError> {
        NullRuntimeHost::list_job_runs_for_gc(self)
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

    fn live_claim_for_worktree_gc(&self, run_id: &str) -> Option<String> {
        NullRuntimeHost::live_claim_for_worktree_gc(self, run_id)
    }

    fn gc_scratch(&self, retention_hours: u64) -> Result<Option<ScratchGcReport>, OrbitError> {
        NullRuntimeHost::gc_scratch(self, retention_hours)
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

    fn record_provider_usage_reading(
        &self,
        observations: &[ProviderLimitObservation],
    ) -> Result<(), OrbitError> {
        NullRuntimeHost::record_provider_usage_reading(self, observations)
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

    fn run_tool_with_context(
        &self,
        name: &str,
        input: Value,
        tool_context: ToolContext,
    ) -> Result<Value, OrbitError> {
        NullRuntimeHost::run_tool_with_context(self, name, input, tool_context)
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
}
