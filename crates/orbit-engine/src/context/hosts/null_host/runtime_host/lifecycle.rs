//! The [`RunLifecycle`] answers of every [`NullRuntimeHost`].

use orbit_common::OrbitError;
use orbit_store::contract::JobRunStepParams;
use orbit_types::task::{
    ContextWideningStep, ExternalRef, Task, TaskArtifact, TaskComment, TaskHistoryEntry,
    TaskPriority, TaskStatus,
};
use orbit_types::workflow::{JobRun, JobRunStartOutcome, JobRunState, PipelineState};

use crate::context::hosts::{
    PluginBrokerHandle, PluginBrokerRun, TaskActivityUpdate, TaskAutomationUpdate,
};

use crate::context::hosts::{NullRuntimeHost, RunLifecycle};

impl<T: NullRuntimeHost> RunLifecycle for T {
    fn start_plugin_broker(
        &self,
        run: &PluginBrokerRun,
    ) -> Result<Option<Box<dyn PluginBrokerHandle>>, OrbitError> {
        NullRuntimeHost::start_plugin_broker(self, run)
    }

    fn register_worker_pid_namespace(&self, pid: u32) -> Result<(), OrbitError> {
        NullRuntimeHost::register_worker_pid_namespace(self, pid)
    }

    fn register_worker_process(&self, pid: u32) -> Result<(), OrbitError> {
        NullRuntimeHost::register_worker_process(self, pid)
    }

    fn worker_invocation(&self) -> Option<orbit_types::tool::WorkerInvocation> {
        NullRuntimeHost::worker_invocation(self)
    }

    fn record_direct_landing_intent(
        &self,
        request: &orbit_types::workflow::automation::DirectLandingRequest,
    ) -> Result<(), OrbitError> {
        NullRuntimeHost::record_direct_landing_intent(self, request)
    }

    fn record_direct_landing(
        &self,
        request: &orbit_types::workflow::automation::DirectLandingRequest,
    ) -> Result<(), OrbitError> {
        NullRuntimeHost::record_direct_landing(self, request)
    }

    fn accepted_automation_coverage(
        &self,
        task_id: &str,
    ) -> Result<Option<orbit_types::workflow::automation::CoverageEvidence>, OrbitError> {
        NullRuntimeHost::accepted_automation_coverage(self, task_id)
    }

    fn insert_job_run(
        &self,
        job_id: &str,
        attempt: u32,
        scheduled_at: chrono::DateTime<chrono::Utc>,
        input: Option<serde_json::Value>,
        retry_source_run_id: Option<String>,
    ) -> Result<JobRun, OrbitError> {
        NullRuntimeHost::insert_job_run(
            self,
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
        started_at: chrono::DateTime<chrono::Utc>,
        pid: u32,
    ) -> Result<JobRunStartOutcome, OrbitError> {
        NullRuntimeHost::mark_job_run_running(self, run_id, started_at, pid)
    }

    fn complete_job_run_step(
        &self,
        run_id: &str,
        params: &JobRunStepParams,
    ) -> Result<bool, OrbitError> {
        NullRuntimeHost::complete_job_run_step(self, run_id, params)
    }

    fn finalize_job_run(
        &self,
        run_id: &str,
        state: JobRunState,
        finished_at: chrono::DateTime<chrono::Utc>,
        duration_ms: Option<u64>,
    ) -> Result<bool, OrbitError> {
        NullRuntimeHost::finalize_job_run(self, run_id, state, finished_at, duration_ms)
    }

    fn get_job_run(&self, run_id: &str) -> Result<Option<JobRun>, OrbitError> {
        NullRuntimeHost::get_job_run(self, run_id)
    }

    fn read_run_state(&self, run_id: &str) -> Result<Option<PipelineState>, OrbitError> {
        NullRuntimeHost::read_run_state(self, run_id)
    }

    fn write_run_state(&self, run_id: &str, state: &PipelineState) -> Result<(), OrbitError> {
        NullRuntimeHost::write_run_state(self, run_id, state)
    }

    fn get_task(&self, task_id: &str) -> Result<Task, OrbitError> {
        NullRuntimeHost::get_task(self, task_id)
    }

    fn get_task_artifact(
        &self,
        task_id: &str,
        path: &str,
    ) -> Result<Option<TaskArtifact>, OrbitError> {
        NullRuntimeHost::get_task_artifact(self, task_id, path)
    }

    fn observe_task_artifact(
        &self,
        task_id: &str,
        path: &str,
    ) -> Result<Option<TaskArtifact>, OrbitError> {
        NullRuntimeHost::observe_task_artifact(self, task_id, path)
    }

    fn get_task_artifacts(&self, task_id: &str) -> Result<Vec<TaskArtifact>, OrbitError> {
        NullRuntimeHost::get_task_artifacts(self, task_id)
    }

    fn get_task_comments(&self, task_id: &str) -> Result<Vec<TaskComment>, OrbitError> {
        NullRuntimeHost::get_task_comments(self, task_id)
    }

    fn get_task_history(&self, task_id: &str) -> Result<Vec<TaskHistoryEntry>, OrbitError> {
        NullRuntimeHost::get_task_history(self, task_id)
    }

    fn get_dependency_task(&self, task_id: &str) -> Result<Option<Task>, OrbitError> {
        NullRuntimeHost::get_dependency_task(self, task_id)
    }

    fn get_dependency_history(
        &self,
        task_id: &str,
    ) -> Result<Option<Vec<TaskHistoryEntry>>, OrbitError> {
        NullRuntimeHost::get_dependency_history(self, task_id)
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
        NullRuntimeHost::list_tasks_filtered(
            self,
            status,
            priority,
            parent_id,
            job_run_id,
            external_ref,
            has_external_ref_system,
        )
    }

    fn list_run_tasks(&self, run_id: &str) -> Result<Vec<Task>, OrbitError> {
        NullRuntimeHost::list_run_tasks(self, run_id)
    }

    fn start_task(
        &self,
        task_id: &str,
        note: Option<String>,
        comment: Option<String>,
    ) -> Result<Task, OrbitError> {
        NullRuntimeHost::start_task(self, task_id, note, comment)
    }

    fn admit_task_for_workflow(&self, task_id: &str, workflow: &str) -> Result<Task, OrbitError> {
        NullRuntimeHost::admit_task_for_workflow(self, task_id, workflow)
    }

    fn update_task_from_activity(
        &self,
        task_id: &str,
        update: TaskActivityUpdate,
    ) -> Result<Task, OrbitError> {
        NullRuntimeHost::update_task_from_activity(self, task_id, update)
    }

    fn apply_task_automation_update(
        &self,
        task_id: &str,
        update: TaskAutomationUpdate,
    ) -> Result<(), OrbitError> {
        NullRuntimeHost::apply_task_automation_update(self, task_id, update)
    }

    fn reject_invalid_finding(
        &self,
        report: &orbit_types::workflow::handoff::FindingInvalidReport,
        base_branch: &str,
    ) -> Result<(), OrbitError> {
        NullRuntimeHost::reject_invalid_finding(self, report, base_branch)
    }

    fn widen_task_context_files(
        &self,
        task_id: &str,
        run_id: &str,
        step: ContextWideningStep,
        activity: &str,
        paths: &[String],
    ) -> Result<Vec<String>, OrbitError> {
        NullRuntimeHost::widen_task_context_files(self, task_id, run_id, step, activity, paths)
    }
}
