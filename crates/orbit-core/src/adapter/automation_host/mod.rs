//! `OrbitRuntime` as the scheduling domain's host port [ORB-12262].
//!
//! The routine, auto-task and delivery/state rules live in `orbit-automation`
//! and evaluate against [`AutomationHost`]. This module is the one place that
//! answers that port for a live runtime, plus the thin auto-task CRUD facade
//! that keeps `runtime.auto_task_*` available to the CLI, the dashboard and
//! the MCP tool host.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use orbit_automation::auto_tasks::crud::{self, AutoTaskAddParams, AutoTaskUpdateParams};
use orbit_automation::host::{AutomationHost, RunOwnerLiveness};
use orbit_automation::source::Source;
use orbit_automation::{AutomationError, members::MemberConstraints};
use orbit_common::OrbitError;
use orbit_store::contracts::{AutomationStoreBackend, TaskCandidates, TaskListFilter};
use orbit_types::identity::Crew;
use orbit_types::task::{
    ArtifactManifestFileV2, Task, TaskAddParams, TaskArtifact, TaskHistoryEntry,
};
use orbit_types::workflow::automation::members::StateTrigger;
use orbit_types::workflow::automation::{AutomationState, SourcePage};
use orbit_types::workflow::{AutoTaskDefinition, JobRun, JobRunTrigger, PipelineState};
use serde_json::Value;

use crate::OrbitRuntime;

impl AutomationHost for OrbitRuntime {
    fn repo_root(&self) -> &Path {
        &self.paths().repo_root
    }

    fn local_orbit_dir(&self) -> PathBuf {
        self.paths().local_dir.clone()
    }

    fn shared_orbit_dir(&self) -> PathBuf {
        self.shared_root()
    }

    fn state_dir(&self) -> PathBuf {
        self.paths().state_dir.clone()
    }

    fn workspace_id(&self) -> Result<String, OrbitError> {
        OrbitRuntime::workspace_id(self)
    }

    fn machine_identity(&self) -> Option<&str> {
        self.automation_machine_identity()
    }

    fn workspace_owner_machine_id(&self) -> Option<&str> {
        OrbitRuntime::workspace_owner_machine_id(self)
    }

    fn coordination_write_owner(&self) -> Option<&str> {
        OrbitRuntime::coordination_write_owner(self)
    }

    fn write_label(&self) -> Result<String, OrbitError> {
        self.actor().resolve_write_label(None, None)
    }

    fn automation_store(&self) -> Result<Arc<dyn AutomationStoreBackend>, OrbitError> {
        OrbitRuntime::automation_store(self)
    }

    fn get_task(&self, id: &str) -> Result<Task, OrbitError> {
        OrbitRuntime::get_task(self, id)
    }

    fn get_task_history(&self, id: &str) -> Result<Vec<TaskHistoryEntry>, OrbitError> {
        OrbitRuntime::get_task_history(self, id)
    }

    fn get_task_artifact(&self, id: &str, path: &str) -> Result<Option<TaskArtifact>, OrbitError> {
        OrbitRuntime::get_task_artifact(self, id, path)
    }

    fn get_task_artifact_manifest(
        &self,
        id: &str,
    ) -> Result<Vec<ArtifactManifestFileV2>, OrbitError> {
        OrbitRuntime::get_task_artifact_manifest(self, id)
    }

    fn task_candidates(
        &self,
        filter: &TaskListFilter,
        limit: usize,
    ) -> Result<TaskCandidates, OrbitError> {
        OrbitRuntime::task_candidates(self, filter, limit)
    }

    fn list_tasks_by_tags(&self, tags: &[String]) -> Result<Vec<Task>, OrbitError> {
        OrbitRuntime::list_tasks_by_tags(self, tags)
    }

    fn add_task(&self, params: TaskAddParams) -> Result<Task, OrbitError> {
        OrbitRuntime::add_task(self, params)
    }

    fn add_task_admitted(
        &self,
        params: TaskAddParams,
        action_key: &str,
    ) -> Result<Task, OrbitError> {
        OrbitRuntime::add_task_admitted(self, params, None, None, Some(action_key))
    }

    fn show_job_run(&self, run_id: &str) -> Result<JobRun, OrbitError> {
        // Keeps the runtime's stale-run reconciliation in the path Automation
        // observes; a raw store read would report a dead run as live.
        OrbitRuntime::show_job_run(self, run_id)
    }

    fn job_run(&self, run_id: &str) -> Result<Option<JobRun>, OrbitError> {
        self.get_job_run_backend(run_id)
    }

    fn job_run_retries(&self, run_id: &str, limit: usize) -> Result<Vec<JobRun>, OrbitError> {
        self.stores().jobs().job_run_retries(run_id, limit)
    }

    fn read_run_state(&self, run_id: &str) -> Result<Option<PipelineState>, OrbitError> {
        OrbitRuntime::read_run_state(self, run_id)
    }

    fn run_owner_liveness(&self, run: &JobRun) -> RunOwnerLiveness {
        match crate::application::job::run_owner_liveness(run) {
            crate::application::job::RunOwnerLiveness::Alive => RunOwnerLiveness::Alive,
            crate::application::job::RunOwnerLiveness::Stopped => RunOwnerLiveness::Stopped,
            crate::application::job::RunOwnerLiveness::Unknown => RunOwnerLiveness::Unknown,
        }
    }

    fn automation_job_for_key(&self, action_key: &str) -> Result<Option<String>, OrbitError> {
        self.stores().jobs().automation_job_for_key(action_key)
    }

    fn submit_automation_run(
        &self,
        job_name: &str,
        input: Value,
        action_key: &str,
    ) -> Result<String, OrbitError> {
        self.submit_automation_pipeline_run(job_name, input, action_key)
            .map(|run| run.run_id)
    }

    fn submit_routine_run(
        &self,
        source_orbit_dir: &Path,
        job_name: &str,
        actor: &str,
        slot: &str,
    ) -> Result<String, OrbitError> {
        // [ORB-11998] Carry the owning workspace's `.orbit` directory into the
        // run explicitly, so the detached worker that ends up executing it can
        // verify its own resolved workspace matches this one — instead of the
        // run silently trusting whatever the worker's cwd/env resolved to.
        let mut input = serde_json::json!({});
        input[crate::application::job::pipeline::ROUTINE_DISPATCH_ORBIT_DIR_FIELD] =
            serde_json::json!(source_orbit_dir.to_string_lossy());
        let routine = actor.strip_prefix("routine/").unwrap_or(actor);
        self.submit_pipeline_run_with_trigger(
            job_name,
            input,
            None,
            Some(actor),
            JobRunTrigger::routine(routine, slot),
        )
        .map(|invoke| invoke.run_id)
    }

    fn job_target_resolves(&self, job_name: &str) -> bool {
        self.load_v2_job_asset_by_name(job_name).is_ok()
    }

    fn ensure_coordination_task_write_permitted(&self) -> Result<(), OrbitError> {
        OrbitRuntime::ensure_coordination_task_write_permitted(self)
    }

    fn member_constraints(&self, trigger: &StateTrigger) -> Result<MemberConstraints, OrbitError> {
        crate::application::operation::member_constraints(self, trigger)
    }

    fn review_exclusions(
        &self,
        source: &Source<'_>,
        state: &AutomationState,
        page: &mut SourcePage,
    ) -> Result<(), AutomationError> {
        crate::application::review::exclusions(self, source, state, page)
    }

    fn effective_crew(&self, task_crew: Option<&str>) -> Result<Crew, OrbitError> {
        self.resolve_crew_for_task(None, task_crew)
    }

    fn validate_required_tools(&self, required_tools: &[String]) -> Result<(), OrbitError> {
        OrbitRuntime::validate_required_tools(self, required_tools).map(|_| ())
    }

    fn validate_crew_name(&self, crew: Option<&str>) -> Result<(), OrbitError> {
        OrbitRuntime::validate_crew_name(self, crew)
    }

    fn provider_pull_requests_argv(
        &self,
        repository: &str,
        commit: &str,
    ) -> Result<Vec<String>, OrbitError> {
        Ok(orbit_tools::github_cli::commit_pull_requests_request(repository, commit)?.args)
    }

    fn refresh_token_scoreboard(&self) -> Result<(), OrbitError> {
        OrbitRuntime::refresh_token_scoreboard(self)
    }
}

/// The auto-task definition surface both the CLI (`orbit auto-task …`) and the
/// MCP tools (`orbit.auto_task.*`) call. Each method is the domain function in
/// `orbit_automation::auto_tasks::crud` bound to this runtime.
impl OrbitRuntime {
    /// Create a new auto-task definition.
    pub fn auto_task_add(
        &self,
        params: AutoTaskAddParams,
    ) -> Result<AutoTaskDefinition, OrbitError> {
        crud::add(self, params)
    }

    /// List every definition in this workspace (stable filename order).
    pub fn auto_task_list(&self) -> Result<Vec<AutoTaskDefinition>, OrbitError> {
        crud::list(self)
    }

    /// Show one definition by name, or `None` if it does not exist.
    pub fn auto_task_show(&self, name: &str) -> Result<Option<AutoTaskDefinition>, OrbitError> {
        crud::show(self, name)
    }

    /// Apply a present-field patch to a definition.
    pub fn auto_task_update(
        &self,
        name: &str,
        params: AutoTaskUpdateParams,
    ) -> Result<AutoTaskDefinition, OrbitError> {
        crud::update(self, name, params)
    }

    /// Enable or disable a definition (the kill-switch).
    pub fn auto_task_toggle(
        &self,
        name: &str,
        enabled: bool,
    ) -> Result<AutoTaskDefinition, OrbitError> {
        crud::toggle(self, name, enabled)
    }

    /// Mint one task from a definition on demand [ORB-10439].
    pub fn auto_task_mint(&self, name: &str) -> Result<Task, OrbitError> {
        crud::mint(self, name)
    }

    /// The id of a still-open instance of `definition`'s prior mints, if any
    /// [ORB-12158].
    pub fn open_auto_task_instance(
        &self,
        definition: &AutoTaskDefinition,
    ) -> Result<Option<String>, OrbitError> {
        orbit_automation::auto_tasks::open_auto_task_instance(self, definition)
    }
}

#[cfg(test)]
mod tests;
