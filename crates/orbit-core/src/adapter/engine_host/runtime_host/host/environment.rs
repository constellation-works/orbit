//! [`HostEnvironment`] for [`OrbitRuntime`]: each method delegates to a sibling
//! helper grouped by concern, or adapts a type on the way through.

use orbit_common::OrbitError;
use orbit_engine::HostEnvironment;
use orbit_store::contract::{InvocationQuery, InvocationRecord};
use orbit_tools::ToolContext;
use orbit_types::identity::AgentModelPair;
use orbit_types::record::OrbitEvent;
use orbit_types::telemetry::ProviderLimitObservation;
use orbit_types::workflow::JobRun;
use serde_json::Value;

use crate::OrbitRuntime;
use crate::runtime::engine::paths::current_repo_root;

use super::private_vcs;
use crate::adapter::engine_host::runtime_host::{
    agent, crew, host_metadata, invocation, validation,
};

impl HostEnvironment for OrbitRuntime {
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

    fn required_validation_commands(&self) -> Vec<String> {
        validation::required_validation_commands(self)
    }

    fn completion_hold_rules(&self) -> Vec<orbit_types::workflow::CompletionHoldRule> {
        // The kernel's accessor, not this trait method of the same name.
        crate::runtime::RuntimeKernel::completion_hold_rules(self).to_vec()
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

    fn activity_wall_clock_timeout_seconds(&self, activity_name: &str) -> Option<u64> {
        self.settings()
            .activity_wall_clock_timeout_seconds(activity_name)
    }

    fn agent_stall_window(&self) -> Option<std::time::Duration> {
        Some(std::time::Duration::from_secs(
            orbit_engine::AGENT_STALL_MINUTES * 60,
        ))
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
        host_metadata::requires_claude_worker_credential()
    }

    fn validation_subprocess_environment(&self) -> orbit_exec::ValidationEnvironment {
        self.validation_environment()
    }

    fn validation_build_output_env(&self) -> Vec<String> {
        self.validation_env_policy().build_output_env.clone()
    }

    fn orbit_registry_root(&self) -> Option<String> {
        Some(self.paths().global_dir.to_string_lossy().into_owned())
    }

    fn orbit_workspace_selector(&self) -> Option<String> {
        host_metadata::orbit_workspace_selector(self)
    }

    fn refresh_persistence_after_cli_provider(&self) -> Result<(), OrbitError> {
        host_metadata::refresh_persistence_after_cli_provider(self)
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
        self.settings().worktree_reclaim().to_vec()
    }

    fn worktree_parked_reclaim_after_hours(&self) -> Option<u64> {
        Some(self.settings().worktree_parked_reclaim_after_hours())
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
        self.data_root_path()
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

    fn record_provider_usage_reading(
        &self,
        observations: &[ProviderLimitObservation],
    ) -> Result<(), OrbitError> {
        invocation::record_provider_usage_reading(self, observations)
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

    fn run_tool_with_context(
        &self,
        name: &str,
        input: Value,
        tool_context: ToolContext,
    ) -> Result<Value, OrbitError> {
        OrbitRuntime::run_tool_with_context(self, name, input, tool_context)
    }

    fn run_private_vcs_operation(
        &self,
        operation: &str,
        input: Value,
    ) -> Result<Value, OrbitError> {
        private_vcs::run_private_vcs_operation(self, operation, input)
    }

    fn actor_model_identity(&self) -> Option<String> {
        agent::actor_model_identity(self)
    }

    fn pr_config(&self) -> orbit_engine::PrConfig {
        crate::runtime::RuntimeKernel::pr_config(self).clone()
    }
}
