//! The [`HostEnvironment`] answers of every [`NullRuntimeHost`].

use orbit_common::OrbitError;
use orbit_store::contract::{InvocationQuery, InvocationRecord};
use orbit_tools::ToolContext;
use orbit_types::identity::AgentModelPair;
use orbit_types::record::OrbitEvent;
use orbit_types::telemetry::ProviderLimitObservation;
use orbit_types::workflow::JobRun;
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

use crate::context::hosts::{PrConfig, ScratchGcReport, WorktreeGcTaskLookup};

use crate::context::hosts::{HostEnvironment, NullRuntimeHost};

impl<T: NullRuntimeHost> HostEnvironment for T {
    fn validation_build_output_env(&self) -> Vec<String> {
        NullRuntimeHost::validation_build_output_env(self)
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

    fn completion_hold_rules(&self) -> Vec<orbit_types::workflow::CompletionHoldRule> {
        NullRuntimeHost::completion_hold_rules(self)
    }

    fn review_baseline_commands(&self) -> Vec<String> {
        NullRuntimeHost::review_baseline_commands(self)
    }

    fn workspace_base_branch(&self) -> String {
        NullRuntimeHost::workspace_base_branch(self)
    }

    fn validation_gates(&self) -> orbit_types::workflow::HostValidationGates {
        NullRuntimeHost::validation_gates(self)
    }

    fn unlocked_path_validation(&self) -> crate::context::UnlockedPathValidation {
        NullRuntimeHost::unlocked_path_validation(self)
    }

    fn generated_paths(&self) -> Vec<crate::context::GeneratedPath> {
        NullRuntimeHost::generated_paths(self)
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

    fn host_os(&self) -> Option<orbit_types::workflow::EvidenceHostOs> {
        NullRuntimeHost::host_os(self)
    }

    fn agent_provider_config(&self) -> HashMap<String, String> {
        NullRuntimeHost::agent_provider_config(self)
    }

    fn activity_wall_clock_timeout_seconds(&self, activity_name: &str) -> Option<u64> {
        NullRuntimeHost::activity_wall_clock_timeout_seconds(self, activity_name)
    }

    fn agent_stall_window(&self) -> Option<std::time::Duration> {
        NullRuntimeHost::agent_stall_window(self)
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

    fn worktree_parked_reclaim_after_hours(&self) -> Option<u64> {
        NullRuntimeHost::worktree_parked_reclaim_after_hours(self)
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

    fn actor_model_identity(&self) -> Option<String> {
        NullRuntimeHost::actor_model_identity(self)
    }

    fn pr_config(&self) -> PrConfig {
        NullRuntimeHost::pr_config(self)
    }
}
