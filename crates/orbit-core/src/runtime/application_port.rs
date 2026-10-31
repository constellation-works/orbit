//! The [`ApplicationPort`] the kernel calls back through, routing each call to
//! the application and adapter operations on [`OrbitRuntime`].

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_runtime::{ApplicationPort, RuntimeKernel};
use orbit_store::AuditEventInsertParams;
use orbit_store::contracts::{ClaimInspection, TaskCandidates, TaskListFilter};
use orbit_types::task::{
    ExecutorOsLimit, ExternalRef, Task, TaskComplexity, TaskHistoryEntry, TaskPriority, TaskStatus,
};
use orbit_types::tool::ToolSessionContext;
use orbit_types::workflow::{JobRun, JobRunState, PipelineState};
use serde_json::Value;

use super::OrbitRuntime;

/// The application operations every kernel this crate builds calls back into.
pub(crate) static APPLICATION_PORT: CoreApplicationPort = CoreApplicationPort;

pub(crate) struct CoreApplicationPort;

impl ApplicationPort for CoreApplicationPort {
    fn get_task(&self, kernel: &RuntimeKernel, id: &str) -> Result<Task, OrbitError> {
        OrbitRuntime::get_task(OrbitRuntime::from_kernel_ref(kernel), id)
    }

    fn get_task_history(
        &self,
        kernel: &RuntimeKernel,
        id: &str,
    ) -> Result<Vec<TaskHistoryEntry>, OrbitError> {
        OrbitRuntime::get_task_history(OrbitRuntime::from_kernel_ref(kernel), id)
    }

    fn list_tasks_filtered(
        &self,
        kernel: &RuntimeKernel,
        status: Option<TaskStatus>,
        priority: Option<TaskPriority>,
        parent_id: Option<&str>,
        job_run_id: Option<&str>,
        external_ref: Option<&ExternalRef>,
        has_external_ref_system: Option<&str>,
    ) -> Result<Vec<Task>, OrbitError> {
        OrbitRuntime::list_tasks_filtered(
            OrbitRuntime::from_kernel_ref(kernel),
            status,
            priority,
            parent_id,
            job_run_id,
            external_ref,
            has_external_ref_system,
        )
    }

    fn list_run_tasks(
        &self,
        kernel: &RuntimeKernel,
        run_id: &str,
    ) -> Result<Vec<Task>, OrbitError> {
        OrbitRuntime::list_run_tasks(OrbitRuntime::from_kernel_ref(kernel), run_id)
    }

    fn task_status_index(
        &self,
        kernel: &RuntimeKernel,
    ) -> Result<BTreeMap<String, TaskStatus>, OrbitError> {
        OrbitRuntime::task_status_index(OrbitRuntime::from_kernel_ref(kernel))
    }

    fn task_candidates(
        &self,
        kernel: &RuntimeKernel,
        filter: &TaskListFilter,
        limit: usize,
    ) -> Result<TaskCandidates, OrbitError> {
        OrbitRuntime::task_candidates(OrbitRuntime::from_kernel_ref(kernel), filter, limit)
    }

    fn inspect_execution_claims(
        &self,
        kernel: &RuntimeKernel,
    ) -> Result<Vec<ClaimInspection>, OrbitError> {
        OrbitRuntime::inspect_execution_claims(OrbitRuntime::from_kernel_ref(kernel))
    }

    fn apply_task_automation_update(
        &self,
        kernel: &RuntimeKernel,
        task_id: &str,
        update: TaskAutomationUpdate,
    ) -> Result<(), OrbitError> {
        <OrbitRuntime as RuntimeHost>::apply_task_automation_update(
            OrbitRuntime::from_kernel_ref(kernel),
            task_id,
            update,
        )
    }

    fn record_audit_event(
        &self,
        kernel: &RuntimeKernel,
        params: &AuditEventInsertParams,
    ) -> Result<(), OrbitError> {
        OrbitRuntime::record_audit_event(OrbitRuntime::from_kernel_ref(kernel), params)
    }

    fn get_job_run_backend(
        &self,
        kernel: &RuntimeKernel,
        run_id: &str,
    ) -> Result<Option<JobRun>, OrbitError> {
        OrbitRuntime::get_job_run_backend(OrbitRuntime::from_kernel_ref(kernel), run_id)
    }

    fn read_run_state(
        &self,
        kernel: &RuntimeKernel,
        run_id: &str,
    ) -> Result<Option<PipelineState>, OrbitError> {
        OrbitRuntime::read_run_state(OrbitRuntime::from_kernel_ref(kernel), run_id)
    }

    fn list_orphaned_running_job_runs(
        &self,
        kernel: &RuntimeKernel,
    ) -> Result<Vec<JobRun>, OrbitError> {
        OrbitRuntime::list_orphaned_running_job_runs(OrbitRuntime::from_kernel_ref(kernel))
    }

    fn list_orphaned_pending_job_runs(
        &self,
        kernel: &RuntimeKernel,
    ) -> Result<Vec<JobRun>, OrbitError> {
        OrbitRuntime::list_orphaned_pending_job_runs(OrbitRuntime::from_kernel_ref(kernel))
    }

    fn reconcile_stale_job_run(
        &self,
        kernel: &RuntimeKernel,
        run: &JobRun,
    ) -> Result<bool, OrbitError> {
        OrbitRuntime::reconcile_stale_job_run(OrbitRuntime::from_kernel_ref(kernel), run)
    }

    fn record_terminal_outcome_conflict(
        &self,
        kernel: &RuntimeKernel,
        run_id: &str,
        recorded: JobRunState,
        reported: JobRunState,
        reported_finished_at: DateTime<Utc>,
    ) {
        OrbitRuntime::record_terminal_outcome_conflict(
            OrbitRuntime::from_kernel_ref(kernel),
            run_id,
            recorded,
            reported,
            reported_finished_at,
        );
    }

    fn record_pipeline_diagnostic_step(
        &self,
        kernel: &RuntimeKernel,
        run: &JobRun,
        started_at: DateTime<Utc>,
        finished_at: DateTime<Utc>,
        error_code: Option<&str>,
        message: &str,
        state: JobRunState,
    ) -> Result<(), OrbitError> {
        OrbitRuntime::record_pipeline_diagnostic_step(
            OrbitRuntime::from_kernel_ref(kernel),
            run,
            started_at,
            finished_at,
            error_code,
            message,
            state,
        )
    }

    fn best_effort_settle_terminal_claimed_leaf(
        &self,
        kernel: &RuntimeKernel,
        run_id: &str,
        diagnostic: Option<(&str, &str)>,
    ) {
        OrbitRuntime::best_effort_settle_terminal_claimed_leaf(
            OrbitRuntime::from_kernel_ref(kernel),
            run_id,
            diagnostic,
        );
    }

    fn best_effort_release_run_review_attempts(
        &self,
        kernel: &RuntimeKernel,
        run_id: &str,
        finished_at: DateTime<Utc>,
    ) {
        OrbitRuntime::best_effort_release_run_review_attempts(
            OrbitRuntime::from_kernel_ref(kernel),
            run_id,
            finished_at,
        );
    }

    fn executor_host_os_limits(
        &self,
        kernel: &RuntimeKernel,
    ) -> Result<BTreeMap<String, ExecutorOsLimit>, OrbitError> {
        OrbitRuntime::executor_host_os_limits(OrbitRuntime::from_kernel_ref(kernel))
    }

    fn execute_worker_projection(
        &self,
        kernel: &RuntimeKernel,
        name: &str,
        input: &Value,
        session: &ToolSessionContext,
    ) -> Result<Option<Value>, OrbitError> {
        OrbitRuntime::execute_worker_projection(
            OrbitRuntime::from_kernel_ref(kernel),
            name,
            input,
            session,
        )
    }

    fn claimed_leaf_redrawn_crew(
        &self,
        kernel: &RuntimeKernel,
        input: &Value,
        task_id: &str,
        crew: Option<&str>,
        crew_source: Option<&str>,
        complexity: Option<TaskComplexity>,
    ) -> Option<String> {
        OrbitRuntime::claimed_leaf_redrawn_crew(
            OrbitRuntime::from_kernel_ref(kernel),
            input,
            task_id,
            crew,
            crew_source,
            complexity,
        )
    }
}
