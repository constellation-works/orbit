//! [`RunLifecycle`] for [`EngineHost`]: each method delegates to a sibling
//! helper grouped by concern, or adapts a type on the way through.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_engine::{
    PluginBrokerHandle, PluginBrokerRun, RunLifecycle, TaskActivityUpdate, TaskAutomationUpdate,
};
use orbit_store::contract::JobRunStepParams;
use orbit_types::task::{
    ContextWideningStep, ExternalRef, Task, TaskComment, TaskHistoryEntry, TaskPriority, TaskStatus,
};
use orbit_types::workflow::{JobRun, JobRunStartOutcome, JobRunState};

use crate::OrbitRuntime;
use crate::adapter::engine_host::runtime_host::{claims, executors, task_automation};

use super::EngineHost;

impl RunLifecycle for EngineHost<'_> {
    fn start_plugin_broker(
        &self,
        run: &PluginBrokerRun,
    ) -> Result<Option<Box<dyn PluginBrokerHandle>>, OrbitError> {
        executors::start_plugin_broker(self.runtime, run)
    }

    fn register_worker_pid_namespace(&self, pid: u32) -> Result<(), OrbitError> {
        executors::register_worker_pid_namespace(self.runtime, pid)
    }

    fn register_worker_process(&self, pid: u32) -> Result<(), OrbitError> {
        crate::runtime::RuntimeKernel::register_worker_process(self.runtime, pid)
    }

    fn worker_invocation(&self) -> Option<orbit_types::tool::WorkerInvocation> {
        OrbitRuntime::worker_invocation(self.runtime).cloned()
    }

    fn record_direct_landing_intent(
        &self,
        request: &orbit_types::workflow::automation::DirectLandingRequest,
    ) -> Result<(), OrbitError> {
        crate::application::automation::record_direct_landing_intent(self.runtime, request)
    }

    fn record_direct_landing(
        &self,
        request: &orbit_types::workflow::automation::DirectLandingRequest,
    ) -> Result<(), OrbitError> {
        claims::record_direct_landing(self.runtime, request)
    }

    fn accepted_automation_coverage(
        &self,
        task_id: &str,
    ) -> Result<Option<orbit_types::workflow::automation::CoverageEvidence>, OrbitError> {
        crate::application::automation::accepted_action_coverage(self.runtime, task_id, Utc::now())
    }

    fn insert_job_run(
        &self,
        job_id: &str,
        attempt: u32,
        scheduled_at: DateTime<Utc>,
        input: Option<serde_json::Value>,
        retry_source_run_id: Option<String>,
    ) -> Result<JobRun, OrbitError> {
        claims::insert_job_run(
            self.runtime,
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
        started_at: DateTime<Utc>,
        pid: u32,
    ) -> Result<JobRunStartOutcome, OrbitError> {
        claims::mark_job_run_running(self.runtime, run_id, started_at, pid)
    }

    fn complete_job_run_step(
        &self,
        run_id: &str,
        params: &JobRunStepParams,
    ) -> Result<bool, OrbitError> {
        claims::complete_job_run_step(self.runtime, run_id, params)
    }

    fn finalize_job_run(
        &self,
        run_id: &str,
        state: JobRunState,
        finished_at: DateTime<Utc>,
        duration_ms: Option<u64>,
    ) -> Result<bool, OrbitError> {
        claims::finalize_job_run(self.runtime, run_id, state, finished_at, duration_ms)
    }

    fn get_job_run(&self, run_id: &str) -> Result<Option<JobRun>, OrbitError> {
        claims::get_job_run(self.runtime, run_id)
    }

    fn read_run_state(
        &self,
        run_id: &str,
    ) -> Result<Option<orbit_types::workflow::PipelineState>, OrbitError> {
        claims::read_run_state(self.runtime, run_id)
    }

    fn write_run_state(
        &self,
        run_id: &str,
        state: &orbit_types::workflow::PipelineState,
    ) -> Result<(), OrbitError> {
        claims::write_run_state(self.runtime, run_id, state)
    }

    fn get_task(&self, task_id: &str) -> Result<Task, OrbitError> {
        OrbitRuntime::get_task(self.runtime, task_id)
    }

    fn get_task_artifact(
        &self,
        task_id: &str,
        path: &str,
    ) -> Result<Option<orbit_types::task::TaskArtifact>, OrbitError> {
        self.runtime.get_task_artifact_inline(task_id, path)
    }

    fn observe_task_artifact(
        &self,
        task_id: &str,
        path: &str,
    ) -> Result<Option<orbit_types::task::TaskArtifact>, OrbitError> {
        // A best-effort diagnostic read decides nothing, so it must not queue
        // behind every admission waiting on the partition. One queued behind
        // dashboard reads stalled a PR completion's CI poll for 62 s.
        orbit_store::TaskCommitBoundary::observe_for_status(|| {
            self.runtime.get_task_artifact_inline(task_id, path)
        })
    }

    fn get_task_artifacts(
        &self,
        task_id: &str,
    ) -> Result<Vec<orbit_types::task::TaskArtifact>, OrbitError> {
        OrbitRuntime::get_task_artifacts(self.runtime, task_id)
    }

    fn get_task_comments(&self, task_id: &str) -> Result<Vec<TaskComment>, OrbitError> {
        OrbitRuntime::get_task_comments(self.runtime, task_id)
    }

    fn get_task_history(&self, task_id: &str) -> Result<Vec<TaskHistoryEntry>, OrbitError> {
        OrbitRuntime::get_task_history(self.runtime, task_id)
    }

    fn get_dependency_task(&self, task_id: &str) -> Result<Option<Task>, OrbitError> {
        OrbitRuntime::get_dependency_task(self.runtime, task_id)
    }

    fn get_dependency_history(
        &self,
        task_id: &str,
    ) -> Result<Option<Vec<TaskHistoryEntry>>, OrbitError> {
        OrbitRuntime::get_dependency_history(self.runtime, task_id)
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
        OrbitRuntime::list_tasks_filtered(
            self.runtime,
            status,
            priority,
            parent_id,
            job_run_id,
            external_ref,
            has_external_ref_system,
        )
    }

    fn list_run_tasks(&self, run_id: &str) -> Result<Vec<Task>, OrbitError> {
        OrbitRuntime::list_run_tasks(self.runtime, run_id)
    }

    fn start_task(
        &self,
        task_id: &str,
        note: Option<String>,
        comment: Option<String>,
    ) -> Result<Task, OrbitError> {
        OrbitRuntime::start_task_as_system(self.runtime, task_id, note, comment)
    }

    /// Admit a task into a pipeline that is about to build its worktree.
    ///
    /// A claimed leaf takes the bound branch [ORB-12616]: the owner already
    /// admitted this task inside the pull transaction, so re-admitting it
    /// locally would either be a no-op on a replica read or, worse, a second
    /// authority for the same work. Instead the trusted worker binding fixes
    /// which task this run may touch, and the owner's own copy of that task is
    /// read back to confirm it is still the admitted, in-progress one. No
    /// binding means no claimed admission: a bound-less process reaching a
    /// claimed leaf has already been refused upstream, and an unbound ordinary
    /// run keeps the pre-existing local admission unchanged.
    fn admit_task_for_workflow(&self, task_id: &str, workflow: &str) -> Result<Task, OrbitError> {
        claims::admit_task_for_workflow(self.runtime, task_id, workflow)
    }

    fn update_task_from_activity(
        &self,
        task_id: &str,
        update: TaskActivityUpdate,
    ) -> Result<Task, OrbitError> {
        claims::update_task_from_activity(self.runtime, task_id, update)
    }

    fn reject_invalid_finding(
        &self,
        report: &orbit_types::workflow::handoff::FindingInvalidReport,
        base_branch: &str,
    ) -> Result<(), OrbitError> {
        task_automation::reject_invalid_finding(self.runtime, report, base_branch)
    }

    fn widen_task_context_files(
        &self,
        task_id: &str,
        run_id: &str,
        step: ContextWideningStep,
        activity: &str,
        paths: &[String],
    ) -> Result<Vec<String>, OrbitError> {
        claims::widen_task_context_files(self.runtime, task_id, run_id, step, activity, paths)
    }

    fn apply_task_automation_update(
        &self,
        task_id: &str,
        update: TaskAutomationUpdate,
    ) -> Result<(), OrbitError> {
        task_automation::apply_task_automation_update(self.runtime, task_id, update)
    }
}
