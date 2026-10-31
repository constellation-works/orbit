//! The use cases the kernel calls back into.
//!
//! A few kernel mechanisms (tool dispatch, audit recording, crew resolution,
//! reservation cleanup and blocking a run's tasks) finish by reading or
//! writing through `orbit-core` application and adapter code. This crate
//! cannot depend on `orbit-core`, so the composition root supplies those
//! operations as an [`ApplicationPort`] when it builds the kernel, and the
//! kernel methods below forward to it under the names its callers already
//! use. Each port method receives the kernel it is called on; the
//! implementation runs the application operation on the `OrbitRuntime` that
//! owns that kernel.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_engine::TaskAutomationUpdate;
use orbit_store::AuditEventInsertParams;
use orbit_store::contracts::{ClaimInspection, TaskCandidates, TaskListFilter};
use orbit_types::task::{
    ExecutorOsLimit, ExternalRef, Task, TaskComplexity, TaskHistoryEntry, TaskPriority, TaskStatus,
};
use orbit_types::tool::ToolSessionContext;
use orbit_types::workflow::{JobRun, JobRunState, PipelineState};
use serde_json::Value;

use crate::RuntimeKernel;

/// Application and adapter operations the kernel calls on the runtime that
/// owns it. Implemented once, by `orbit-core`'s composition root.
pub trait ApplicationPort: Send + Sync {
    fn get_task(&self, kernel: &RuntimeKernel, id: &str) -> Result<Task, OrbitError>;
    fn get_task_history(
        &self,
        kernel: &RuntimeKernel,
        id: &str,
    ) -> Result<Vec<TaskHistoryEntry>, OrbitError>;
    #[allow(clippy::too_many_arguments)]
    fn list_tasks_filtered(
        &self,
        kernel: &RuntimeKernel,
        status: Option<TaskStatus>,
        priority: Option<TaskPriority>,
        parent_id: Option<&str>,
        job_run_id: Option<&str>,
        external_ref: Option<&ExternalRef>,
        has_external_ref_system: Option<&str>,
    ) -> Result<Vec<Task>, OrbitError>;
    fn list_run_tasks(&self, kernel: &RuntimeKernel, run_id: &str)
    -> Result<Vec<Task>, OrbitError>;
    fn task_status_index(
        &self,
        kernel: &RuntimeKernel,
    ) -> Result<BTreeMap<String, TaskStatus>, OrbitError>;
    fn task_candidates(
        &self,
        kernel: &RuntimeKernel,
        filter: &TaskListFilter,
        limit: usize,
    ) -> Result<TaskCandidates, OrbitError>;
    fn inspect_execution_claims(
        &self,
        kernel: &RuntimeKernel,
    ) -> Result<Vec<ClaimInspection>, OrbitError>;
    fn apply_task_automation_update(
        &self,
        kernel: &RuntimeKernel,
        task_id: &str,
        update: TaskAutomationUpdate,
    ) -> Result<(), OrbitError>;
    fn record_audit_event(
        &self,
        kernel: &RuntimeKernel,
        params: &AuditEventInsertParams,
    ) -> Result<(), OrbitError>;
    fn get_job_run_backend(
        &self,
        kernel: &RuntimeKernel,
        run_id: &str,
    ) -> Result<Option<JobRun>, OrbitError>;
    fn read_run_state(
        &self,
        kernel: &RuntimeKernel,
        run_id: &str,
    ) -> Result<Option<PipelineState>, OrbitError>;
    fn list_orphaned_running_job_runs(
        &self,
        kernel: &RuntimeKernel,
    ) -> Result<Vec<JobRun>, OrbitError>;
    fn list_orphaned_pending_job_runs(
        &self,
        kernel: &RuntimeKernel,
    ) -> Result<Vec<JobRun>, OrbitError>;
    fn reconcile_stale_job_run(
        &self,
        kernel: &RuntimeKernel,
        run: &JobRun,
    ) -> Result<bool, OrbitError>;
    fn record_terminal_outcome_conflict(
        &self,
        kernel: &RuntimeKernel,
        run_id: &str,
        recorded: JobRunState,
        reported: JobRunState,
        reported_finished_at: DateTime<Utc>,
    );
    #[allow(clippy::too_many_arguments)]
    fn record_pipeline_diagnostic_step(
        &self,
        kernel: &RuntimeKernel,
        run: &JobRun,
        started_at: DateTime<Utc>,
        finished_at: DateTime<Utc>,
        error_code: Option<&str>,
        message: &str,
        state: JobRunState,
    ) -> Result<(), OrbitError>;
    fn best_effort_settle_terminal_claimed_leaf(
        &self,
        kernel: &RuntimeKernel,
        run_id: &str,
        diagnostic: Option<(&str, &str)>,
    );
    fn best_effort_release_run_review_attempts(
        &self,
        kernel: &RuntimeKernel,
        run_id: &str,
        finished_at: DateTime<Utc>,
    );
    fn executor_host_os_limits(
        &self,
        kernel: &RuntimeKernel,
    ) -> Result<BTreeMap<String, ExecutorOsLimit>, OrbitError>;
    fn execute_worker_projection(
        &self,
        kernel: &RuntimeKernel,
        name: &str,
        input: &Value,
        session: &ToolSessionContext,
    ) -> Result<Option<Value>, OrbitError>;
    #[allow(clippy::too_many_arguments)]
    fn claimed_leaf_redrawn_crew(
        &self,
        kernel: &RuntimeKernel,
        input: &Value,
        task_id: &str,
        crew: Option<&str>,
        crew_source: Option<&str>,
        complexity: Option<TaskComplexity>,
    ) -> Option<String>;
}

impl RuntimeKernel {
    pub(crate) fn get_task(&self, id: &str) -> Result<Task, OrbitError> {
        self.application.get_task(self, id)
    }

    pub(crate) fn get_task_history(&self, id: &str) -> Result<Vec<TaskHistoryEntry>, OrbitError> {
        self.application.get_task_history(self, id)
    }

    pub(crate) fn list_tasks_filtered(
        &self,
        status: Option<TaskStatus>,
        priority: Option<TaskPriority>,
        parent_id: Option<&str>,
        job_run_id: Option<&str>,
        external_ref: Option<&ExternalRef>,
        has_external_ref_system: Option<&str>,
    ) -> Result<Vec<Task>, OrbitError> {
        self.application.list_tasks_filtered(
            self,
            status,
            priority,
            parent_id,
            job_run_id,
            external_ref,
            has_external_ref_system,
        )
    }

    pub(crate) fn list_run_tasks(&self, run_id: &str) -> Result<Vec<Task>, OrbitError> {
        self.application.list_run_tasks(self, run_id)
    }

    pub(crate) fn task_status_index(&self) -> Result<BTreeMap<String, TaskStatus>, OrbitError> {
        self.application.task_status_index(self)
    }

    pub(crate) fn task_candidates(
        &self,
        filter: &TaskListFilter,
        limit: usize,
    ) -> Result<TaskCandidates, OrbitError> {
        self.application.task_candidates(self, filter, limit)
    }

    pub(crate) fn inspect_execution_claims(&self) -> Result<Vec<ClaimInspection>, OrbitError> {
        self.application.inspect_execution_claims(self)
    }

    pub(crate) fn apply_task_automation_update(
        &self,
        task_id: &str,
        update: TaskAutomationUpdate,
    ) -> Result<(), OrbitError> {
        self.application
            .apply_task_automation_update(self, task_id, update)
    }

    pub(crate) fn record_audit_event(
        &self,
        params: &AuditEventInsertParams,
    ) -> Result<(), OrbitError> {
        self.application.record_audit_event(self, params)
    }

    pub(crate) fn get_job_run_backend(&self, run_id: &str) -> Result<Option<JobRun>, OrbitError> {
        self.application.get_job_run_backend(self, run_id)
    }

    pub(crate) fn read_run_state(&self, run_id: &str) -> Result<Option<PipelineState>, OrbitError> {
        self.application.read_run_state(self, run_id)
    }

    pub(crate) fn list_orphaned_running_job_runs(&self) -> Result<Vec<JobRun>, OrbitError> {
        self.application.list_orphaned_running_job_runs(self)
    }

    pub(crate) fn list_orphaned_pending_job_runs(&self) -> Result<Vec<JobRun>, OrbitError> {
        self.application.list_orphaned_pending_job_runs(self)
    }

    pub(crate) fn reconcile_stale_job_run(&self, run: &JobRun) -> Result<bool, OrbitError> {
        self.application.reconcile_stale_job_run(self, run)
    }

    pub(crate) fn record_terminal_outcome_conflict(
        &self,
        run_id: &str,
        recorded: JobRunState,
        reported: JobRunState,
        reported_finished_at: DateTime<Utc>,
    ) {
        self.application.record_terminal_outcome_conflict(
            self,
            run_id,
            recorded,
            reported,
            reported_finished_at,
        );
    }

    pub(crate) fn record_pipeline_diagnostic_step(
        &self,
        run: &JobRun,
        started_at: DateTime<Utc>,
        finished_at: DateTime<Utc>,
        error_code: Option<&str>,
        message: &str,
        state: JobRunState,
    ) -> Result<(), OrbitError> {
        self.application.record_pipeline_diagnostic_step(
            self,
            run,
            started_at,
            finished_at,
            error_code,
            message,
            state,
        )
    }

    pub(crate) fn best_effort_settle_terminal_claimed_leaf(
        &self,
        run_id: &str,
        diagnostic: Option<(&str, &str)>,
    ) {
        self.application
            .best_effort_settle_terminal_claimed_leaf(self, run_id, diagnostic);
    }

    pub(crate) fn best_effort_release_run_review_attempts(
        &self,
        run_id: &str,
        finished_at: DateTime<Utc>,
    ) {
        self.application
            .best_effort_release_run_review_attempts(self, run_id, finished_at);
    }

    pub(crate) fn executor_host_os_limits(
        &self,
    ) -> Result<BTreeMap<String, ExecutorOsLimit>, OrbitError> {
        self.application.executor_host_os_limits(self)
    }

    pub(crate) fn execute_worker_projection(
        &self,
        name: &str,
        input: &Value,
        session: &ToolSessionContext,
    ) -> Result<Option<Value>, OrbitError> {
        self.application
            .execute_worker_projection(self, name, input, session)
    }

    pub(crate) fn claimed_leaf_redrawn_crew(
        &self,
        input: &Value,
        task_id: &str,
        crew: Option<&str>,
        crew_source: Option<&str>,
        complexity: Option<TaskComplexity>,
    ) -> Option<String> {
        self.application.claimed_leaf_redrawn_crew(
            self,
            input,
            task_id,
            crew,
            crew_source,
            complexity,
        )
    }
}
