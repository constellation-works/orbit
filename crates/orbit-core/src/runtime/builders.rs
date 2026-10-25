//! The kernel's builders and derived runtimes, returning [`OrbitRuntime`].

use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_runtime::{context, host_resource, host_signal, task_pr_forge, workspace};
use orbit_types::task::HostOs;
use orbit_types::tool::WorkerInvocation;

use super::OrbitRuntime;

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
