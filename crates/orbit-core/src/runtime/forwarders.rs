//! Inherent forwarders to kernel methods, and kernel functions consumer crates
//! call by path.

use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_runtime::RuntimeKernel;
use orbit_store::{InvocationQuery, InvocationRecord};
use orbit_types::record::OrbitEvent;
use orbit_types::task::HostOs;
use orbit_types::telemetry::ProviderLimitObservation;
use orbit_types::tool::WorkerInvocation;

use super::OrbitRuntime;

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
