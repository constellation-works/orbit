//! Host lifecycle, forge, and resource probes.

use std::path::PathBuf;
use std::sync::Arc;

use super::OrbitRuntime;
use crate::runtime::host_resource::{
    HostResourceMonitor, HostResourceProbe, HostResourceStatus, ResourceAdmission,
};
use crate::runtime::host_signal::{HostSignalProbe, ScheduledShutdown};
use crate::runtime::task_pr_forge::TaskPrForge;

impl OrbitRuntime {
    /// Admit tasks as a host running `os` would (`None`: an OS outside the
    /// `os:` namespace, which runs only untagged tasks). A fixture composes
    /// two runtimes in one process and gives each the OS it stands in for.
    pub fn with_host_os(mut self, os: Option<orbit_types::task::HostOs>) -> Self {
        self.host_os = os;
        self
    }

    /// The operating system this runtime admits tasks for: what local
    /// admission checks a task's `os:` tags against, and what a pull drain
    /// declares to its owner.
    pub fn host_os(&self) -> Option<orbit_types::task::HostOs> {
        self.host_os
    }

    /// Replace the host-signal probe, so a fixture can present a scheduled
    /// shutdown without touching the real `/run` filesystem [ORB-12968].
    pub fn with_host_signal_probe(mut self, probe: Arc<dyn HostSignalProbe>) -> Self {
        self.host_signals = probe;
        self
    }

    /// Replace the forge a terminal task decision closes PRs through, so a
    /// fixture can observe closures without a real repository.
    pub fn with_task_pr_forge(mut self, forge: Arc<dyn TaskPrForge>) -> Self {
        self.task_pr_forge = forge;
        self
    }

    pub(crate) fn task_pr_forge(&self) -> &dyn TaskPrForge {
        self.task_pr_forge.as_ref()
    }

    /// The host shutdown or reboot currently pending, if any. While one is,
    /// unattended admission points start no new runs; in-flight runs are
    /// left alone.
    pub fn scheduled_host_shutdown(&self) -> Option<ScheduledShutdown> {
        self.host_signals.scheduled_shutdown()
    }

    /// Replace the resource probe for deterministic pressure/unknown fixtures.
    pub fn with_host_resource_probe(mut self, probe: Arc<dyn HostResourceProbe>) -> Self {
        self.host_resources = Arc::new(
            HostResourceMonitor::new(probe, self.context.settings().resource_throttle().clone())
                .with_shared_history(&self.global_root()),
        );
        self
    }

    /// Shared sampler/evaluator; fresh runtimes recover recent host history too.
    pub fn host_resource_monitor(&self) -> Arc<HostResourceMonitor> {
        Arc::clone(&self.host_resources)
    }

    /// Resource verdict for the host and filesystems used by this workspace.
    /// Admission consumers may consult it; this method changes no run state.
    pub fn host_resource_status(&self) -> HostResourceStatus {
        self.host_resources.snapshot(&self.host_resource_paths())
    }

    /// Whether host pressure holds new admissions now [ORB-13901]. Drains,
    /// pull drains and ship discovery start no new task while it throttles;
    /// running work is never touched. Unknown telemetry fails open.
    pub fn resource_admission(&self) -> ResourceAdmission {
        self.host_resources.admission(&self.host_resource_paths())
    }

    fn host_resource_paths(&self) -> Vec<PathBuf> {
        let mut paths = vec![
            self.paths().repo_root.clone(),
            self.shared_root().join("state/worktrees"),
            self.global_root(),
        ];
        paths.sort();
        paths.dedup();
        paths
    }
}
