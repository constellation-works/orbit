//! [`HostEnvironment`] for [`EngineHost`]: each method delegates to a sibling
//! helper grouped by concern, or adapts a type on the way through.

use std::sync::Arc;

use orbit_common::OrbitError;
use orbit_engine::{HostEnvironment, RunSettings, RuntimeHostRunSettings};
use orbit_store::contract::{InvocationQuery, InvocationRecord};
use orbit_tools::ToolContext;
use orbit_types::identity::AgentModelPair;
use orbit_types::record::OrbitEvent;
use orbit_types::telemetry::ProviderLimitObservation;
use orbit_types::workflow::JobRun;
use serde_json::Value;

use crate::OrbitRuntime;
use crate::adapter::engine_host::runtime_host::{
    agent, crew, host_metadata, invocation, validation,
};
use crate::app::gc;

use super::{EngineHost, private_vcs};

impl RuntimeHostRunSettings for EngineHost<'_> {
    fn run_settings(&self) -> Arc<RunSettings> {
        Arc::clone(&self.run_settings)
    }
}

impl HostEnvironment for EngineHost<'_> {
    fn admit_owed_criteria(
        &self,
        task_id: &str,
        run_id: &str,
        owed: &[orbit_types::workflow::OwedCriterion],
    ) -> Result<(), OrbitError> {
        validation::admit_owed_criteria(self.runtime, task_id, run_id, owed)
    }

    fn outstanding_owed_criteria(
        &self,
        task_id: &str,
        run_id: Option<&str>,
    ) -> Result<Vec<usize>, OrbitError> {
        validation::outstanding_owed_criteria(self.runtime, task_id, run_id)
    }

    fn required_validation_commands(&self) -> Vec<String> {
        self.runtime
            .host_validation_gates()
            .gates
            .required_validation_commands
    }

    fn review_baseline_commands(&self) -> Vec<String> {
        self.runtime.host_validation_gates().gates.baseline_commands
    }

    fn validation_gates(&self) -> orbit_types::workflow::HostValidationGates {
        self.runtime.host_validation_gates()
    }

    fn record_validated_tree(&self, tree: &orbit_engine::ValidatedTree) -> Result<(), OrbitError> {
        validation::record_validated_tree(self.runtime, tree)
    }

    fn validated_tree(
        &self,
        run_id: &str,
    ) -> Result<Option<orbit_engine::ValidatedTree>, OrbitError> {
        validation::validated_tree(self.runtime, run_id)
    }

    fn agent_provider_config(&self) -> std::collections::HashMap<String, String> {
        agent::agent_provider_config(self.runtime)
    }

    fn agent_subprocess_environment(&self, required_env_vars: &[&str]) -> Vec<(String, String)> {
        self.runtime.agent_session_environment(required_env_vars)
    }

    fn crew_agent_subprocess_environment(
        &self,
        crew_env: &[orbit_types::identity::CrewEnvVar],
        required_env_vars: &[&str],
    ) -> Vec<(String, String)> {
        self.runtime
            .crew_agent_session_environment(crew_env, required_env_vars)
    }

    fn validation_subprocess_environment(&self) -> orbit_exec::ValidationEnvironment {
        self.runtime.validation_environment()
    }

    fn refresh_persistence_after_cli_provider(&self) -> Result<(), OrbitError> {
        host_metadata::refresh_persistence_after_cli_provider(self.runtime)
    }

    fn record_event(&self, event: OrbitEvent) -> Result<(), OrbitError> {
        crate::runtime::RuntimeKernel::record_event(self.runtime, event)
    }

    fn list_job_runs_for_gc(&self) -> Result<Vec<JobRun>, OrbitError> {
        gc::list_job_runs_for_worktree_gc(self.runtime)
    }

    fn lookup_task_for_worktree_gc(
        &self,
        run_id: &str,
        task_id: &str,
    ) -> orbit_engine::WorktreeGcTaskLookup {
        gc::worktree_gc_task_lookup(self.runtime, run_id, task_id)
    }

    fn worktree_gc_task_lookup_scope(&self, run_id: &str) -> Option<String> {
        gc::worktree_gc_task_lookup_scope(self.runtime, run_id)
    }

    fn settled_claim_for_worktree_gc(&self, run_id: &str) -> Option<String> {
        gc::worktree_gc_settled_claim(self.runtime, run_id)
    }

    fn live_claim_for_worktree_gc(&self, run_id: &str) -> Option<String> {
        gc::worktree_gc_live_claim(self.runtime, run_id)
    }

    fn gc_scratch(
        &self,
        retention_hours: u64,
    ) -> Result<Option<orbit_engine::ScratchGcReport>, OrbitError> {
        gc::gc_scratch_by_age(self.runtime, retention_hours).map(Some)
    }

    fn cancel_job_run(&self, run_id: &str) -> Result<(), OrbitError> {
        OrbitRuntime::cancel_job_run(self.runtime, run_id).map(|_| ())
    }

    fn resolved_agent_model_pair(&self, agent_cli: &str) -> Option<AgentModelPair> {
        invocation::resolved_agent_model_pair(self.runtime, agent_cli)
    }

    fn invocation_records(
        &self,
        query: InvocationQuery,
    ) -> Result<Vec<InvocationRecord>, OrbitError> {
        invocation::invocation_records(self.runtime, query)
    }

    fn record_provider_limit(
        &self,
        observation: &ProviderLimitObservation,
    ) -> Result<(), OrbitError> {
        invocation::record_provider_limit(self.runtime, observation)
    }

    fn record_provider_usage_reading(
        &self,
        observations: &[ProviderLimitObservation],
    ) -> Result<(), OrbitError> {
        invocation::record_provider_usage_reading(self.runtime, observations)
    }

    fn activity_implementer_identity(
        &self,
        input: &Value,
    ) -> Result<(Option<String>, Option<String>), OrbitError> {
        invocation::activity_implementer_identity(self.runtime, input)
    }

    fn resolved_crew_model(&self, run_id: &str) -> Result<Option<String>, OrbitError> {
        crew::resolved_crew_model(self.runtime, run_id)
    }

    fn run_tool_with_context(
        &self,
        name: &str,
        input: Value,
        tool_context: ToolContext,
    ) -> Result<Value, OrbitError> {
        OrbitRuntime::run_tool_with_context(self.runtime, name, input, tool_context)
    }

    fn run_private_vcs_operation(
        &self,
        operation: &str,
        input: Value,
    ) -> Result<Value, OrbitError> {
        private_vcs::run_private_vcs_operation(self, operation, input)
    }
}
