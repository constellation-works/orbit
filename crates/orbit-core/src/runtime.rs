//! The runtime kernel, re-exported from `orbit-runtime`, and the public
//! [`OrbitRuntime`] that wraps it.
//!
//! `crate::runtime::…` and `orbit_core::runtime::…` paths resolve to the
//! kernel crate's items. [`OrbitRuntime`] owns a [`RuntimeKernel`] and
//! dereferences to it, so kernel methods are called on it directly, while
//! application and adapter methods stay inherent on this crate's type. The
//! kernel reaches those methods only through [`ApplicationPort`], which this
//! module implements.

use std::collections::BTreeMap;
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
pub use orbit_runtime::*;
use orbit_store::contracts::{ClaimInspection, TaskCandidates, TaskListFilter};
use orbit_store::{AuditEventInsertParams, InvocationQuery, InvocationRecord};
use orbit_types::record::OrbitEvent;
use orbit_types::task::{
    ExecutorOsLimit, ExternalRef, HostOs, Task, TaskComplexity, TaskHistoryEntry, TaskPriority,
    TaskStatus,
};
use orbit_types::telemetry::ProviderLimitObservation;
use orbit_types::tool::{ToolSessionContext, WorkerInvocation};
use orbit_types::workflow::{JobRun, JobRunState, PipelineState};
use serde_json::Value;

#[cfg(test)]
mod tests;

/// The fully initialized Orbit runtime: the [`RuntimeKernel`] plus the
/// application and adapter operations defined on it in this crate.
#[derive(Clone)]
// Transparent, so a kernel reference can be viewed as the runtime that owns it.
#[repr(transparent)]
pub struct OrbitRuntime {
    kernel: RuntimeKernel,
}

impl Deref for OrbitRuntime {
    type Target = RuntimeKernel;

    fn deref(&self) -> &RuntimeKernel {
        &self.kernel
    }
}

impl DerefMut for OrbitRuntime {
    fn deref_mut(&mut self) -> &mut RuntimeKernel {
        &mut self.kernel
    }
}

impl OrbitRuntime {
    pub(crate) fn from_kernel(kernel: RuntimeKernel) -> Self {
        Self { kernel }
    }

    /// The kernel this runtime owns, for a kernel interface such as
    /// [`WorkspaceCatalog::open`](workspace::catalog::WorkspaceCatalog::open)
    /// that returns one.
    pub fn into_kernel(self) -> RuntimeKernel {
        self.kernel
    }

    fn from_kernel_ref(kernel: &RuntimeKernel) -> &Self {
        // SAFETY: `OrbitRuntime` is `repr(transparent)` over its only field,
        // a `RuntimeKernel`, so both types share one layout and the cast
        // keeps the reference's lifetime and validity.
        unsafe { &*(kernel as *const RuntimeKernel).cast::<Self>() }
    }
}

// The kernel's builders and derived runtimes, returning this crate's type so
// application and adapter methods stay callable on the result.
impl OrbitRuntime {
    pub fn with_worker_invocation(
        self,
        invocation: WorkerInvocation,
        coordinator: Arc<dyn orbit_tools::OwnerCoordinator>,
    ) -> Result<Self, OrbitError> {
        self.kernel
            .with_worker_invocation(invocation, coordinator)
            .map(Self::from_kernel)
    }

    pub fn without_worker_routing(&self) -> Self {
        Self::from_kernel(self.kernel.without_worker_routing())
    }

    pub fn with_owner_coordinator(
        self,
        coordinator: Arc<dyn orbit_tools::OwnerCoordinator>,
    ) -> Self {
        Self::from_kernel(self.kernel.with_owner_coordinator(coordinator))
    }

    pub fn with_drain_owner_transport(
        self,
        transport: Arc<dyn orbit_tools::DrainOwnerTransport>,
    ) -> Self {
        Self::from_kernel(self.kernel.with_drain_owner_transport(transport))
    }

    pub fn with_host_os(self, os: Option<HostOs>) -> Self {
        Self::from_kernel(self.kernel.with_host_os(os))
    }

    pub fn with_registered_host_oses(
        self,
        hosts: impl IntoIterator<Item = (String, HostOs)>,
    ) -> Self {
        Self::from_kernel(self.kernel.with_registered_host_oses(hosts))
    }

    pub fn with_browser_kit_home(self, home: &Path) -> Self {
        Self::from_kernel(self.kernel.with_browser_kit_home(home))
    }

    pub fn with_host_signal_probe(self, probe: Arc<dyn host_signal::HostSignalProbe>) -> Self {
        Self::from_kernel(self.kernel.with_host_signal_probe(probe))
    }

    pub fn with_host_shutdown_hold_minutes(self, minutes: u32) -> Self {
        Self::from_kernel(self.kernel.with_host_shutdown_hold_minutes(minutes))
    }

    pub fn with_task_pr_forge(self, forge: Arc<dyn task_pr_forge::TaskPrForge>) -> Self {
        Self::from_kernel(self.kernel.with_task_pr_forge(forge))
    }

    pub fn with_host_resource_probe(
        self,
        probe: Arc<dyn host_resource::HostResourceProbe>,
    ) -> Self {
        Self::from_kernel(self.kernel.with_host_resource_probe(probe))
    }

    pub fn with_host_load_proc_root(self, proc_root: PathBuf) -> Self {
        Self::from_kernel(self.kernel.with_host_load_proc_root(proc_root))
    }

    pub fn with_actor(self, actor: context::ActorIdentity) -> Self {
        Self::from_kernel(self.kernel.with_actor(actor))
    }

    pub fn with_automation_machine_identity(self, machine_id: Option<String>) -> Self {
        Self::from_kernel(self.kernel.with_automation_machine_identity(machine_id))
    }

    pub fn with_automation_execution_location(
        self,
        location: Option<orbit_types::task::ExecutionLocation>,
    ) -> Self {
        Self::from_kernel(self.kernel.with_automation_execution_location(location))
    }

    pub fn with_coordination_write_owner(self, owner_machine_id: Option<String>) -> Self {
        Self::from_kernel(self.kernel.with_coordination_write_owner(owner_machine_id))
    }

    pub fn with_workspace_catalog(
        self,
        catalog: Arc<dyn workspace::catalog::WorkspaceCatalog>,
    ) -> Self {
        Self::from_kernel(self.kernel.with_workspace_catalog(catalog))
    }

    pub fn admission_policy_snapshot(&self) -> Self {
        Self::from_kernel(self.kernel.admission_policy_snapshot())
    }

    pub fn with_ci_failure_time(self, now: DateTime<Utc>) -> Self {
        Self::from_kernel(self.kernel.with_ci_failure_time(now))
    }

    #[cfg(feature = "test-support")]
    pub fn with_claim_known_red_budget(self, budget: std::time::Duration) -> Self {
        Self::from_kernel(self.kernel.with_claim_known_red_budget(budget))
    }
}

// These kernel methods share a name with a method of a trait this crate
// implements for `OrbitRuntime` (`RuntimeHost`, `PipelineRunHost`). Method
// calls find a trait method on `OrbitRuntime` before a kernel method behind
// `Deref`, so these inherent forwarders keep such calls on the kernel method.
impl OrbitRuntime {
    pub fn data_root(&self) -> PathBuf {
        self.kernel.data_root()
    }

    pub fn host_os(&self) -> Option<HostOs> {
        self.kernel.host_os()
    }

    pub fn invocation_records(
        &self,
        filter: InvocationQuery,
    ) -> Result<Vec<InvocationRecord>, OrbitError> {
        self.kernel.invocation_records(filter)
    }

    pub fn pr_config(&self) -> &orbit_engine::PrConfig {
        self.kernel.pr_config()
    }

    pub fn record_event(&self, event: OrbitEvent) -> Result<(), OrbitError> {
        self.kernel.record_event(event)
    }

    pub fn record_provider_limit(
        &self,
        observation: &ProviderLimitObservation,
    ) -> Result<bool, OrbitError> {
        self.kernel.record_provider_limit(observation)
    }

    pub fn review_baseline_commands(&self) -> &[String] {
        self.kernel.review_baseline_commands()
    }

    pub fn worker_invocation(&self) -> Option<&WorkerInvocation> {
        self.kernel.worker_invocation()
    }
}

// Kernel functions consumer crates call by path (`OrbitRuntime::name(..)`),
// which does not reach through `Deref`.
impl OrbitRuntime {
    pub fn current_worker_invocation(
        global_root: &Path,
    ) -> Result<Option<WorkerInvocation>, OrbitError> {
        RuntimeKernel::current_worker_invocation(global_root)
    }

    pub fn list_v2_audit_events(
        &self,
        filter: orbit_store::V2AuditEventFilter,
    ) -> Result<Vec<orbit_store::V2AuditEventRow>, OrbitError> {
        self.kernel.list_v2_audit_events(filter)
    }
}

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
