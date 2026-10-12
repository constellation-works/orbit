//! Host lifecycle, forge, and resource probes.

use std::path::PathBuf;
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::{
    AdmissionLoadQuery, AdmissionLoadStoreBackend, NewAdmissionReservation,
};
use orbit_types::task::NO_DIFF_EXPECTED_TAG;

use super::OrbitRuntime;
use crate::runtime::host_resource::delivery::delivery_jobs;
use crate::runtime::host_resource::forecast::{
    ForecastDecision, HostLoadForecast, HostLoadProbe, RESERVATION_TTL_MINUTES, build_forecast,
};
use crate::runtime::host_resource::profile::{
    ProfileBounds, ProfileLookup, ProfileMatch, ProfileSet,
};
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

    /// Override `machine.host_shutdown_hold_minutes` for a fixture. Production
    /// runtimes take the admitted setting from the global config.
    pub fn with_host_shutdown_hold_minutes(mut self, minutes: u32) -> Self {
        self.context.set_host_shutdown_hold_minutes(minutes);
        self
    }

    /// The admitted shutdown-hold horizon, in minutes.
    pub fn host_shutdown_hold_minutes(&self) -> u32 {
        self.context.settings().host_shutdown_hold_minutes()
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

    /// The host shutdown or reboot currently pending, if any. This is the raw
    /// schedule: status surfaces name it however far away it is. Unattended
    /// admission holds only inside [`Self::host_shutdown_hold_minutes`];
    /// in-flight runs are left alone either way.
    pub fn scheduled_host_shutdown(&self) -> Option<ScheduledShutdown> {
        self.host_signals.scheduled_shutdown()
    }

    /// The pending shutdown when it is close enough to hold new admissions.
    pub fn host_shutdown_admission_hold(&self) -> Option<ScheduledShutdown> {
        self.host_shutdown_admission_hold_at(Utc::now())
    }

    /// [`Self::host_shutdown_admission_hold`] judged at `now`, so a sweep tick
    /// and a test share one clock.
    pub fn host_shutdown_admission_hold_at(&self, now: DateTime<Utc>) -> Option<ScheduledShutdown> {
        self.scheduled_host_shutdown()
            .filter(|shutdown| shutdown.holds_admission(now, self.host_shutdown_hold_minutes()))
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

    /// Every resource profile on this host, across its workspaces. Built from
    /// the stored delivery minutes and kept, across calls and clones, until a
    /// sampled run finishes or the `[machine.auto_concurrency]` bounds change.
    pub fn resource_profiles(&self) -> Result<Arc<ProfileSet>, OrbitError> {
        let store = self.sqlite_store()?;
        let tasks = self.stores().tasks();
        self.resource_profiles.profiles(
            &store,
            &|task_ids| tasks.task_ids_with_tag(NO_DIFF_EXPECTED_TAG, task_ids),
            ProfileBounds::from(self.context.settings().auto_concurrency()),
            Utc::now(),
        )
    }

    /// The profile describing one delivery, and the fallback step that found
    /// it.
    pub fn resource_profile_for(&self, lookup: &ProfileLookup) -> Result<ProfileMatch, OrbitError> {
        Ok(self.resource_profiles()?.lookup(lookup))
    }

    /// Read the host's load from `<proc_root>/{stat,meminfo}` instead of
    /// `/proc`, so a fixture can present a measured host.
    pub fn with_host_load_proc_root(mut self, proc_root: PathBuf) -> Self {
        self.host_load = Arc::new(HostLoadProbe::new(proc_root));
        self
    }

    /// Decide admissions against the host load forecast, serialized with
    /// every other admitter on this host: under the host admission lock,
    /// build the forecast, let `decide` choose, and reserve each task it
    /// admitted, so the next admitter in any process counts it before its run
    /// exists.
    ///
    /// The running set, reservations and baseline are read fresh inside the
    /// lock on every call; profiles come from the profile cache. `decide`
    /// runs while the store is locked for writing: it must only compute over
    /// the forecast, and not call the store, the network or a subprocess.
    pub fn admit_against_forecast<T>(
        &self,
        decide: impl FnOnce(&HostLoadForecast) -> Result<ForecastDecision<T>, OrbitError>,
    ) -> Result<T, OrbitError> {
        self.admit_against_forecast_at(Utc::now(), decide)
    }

    /// [`Self::admit_against_forecast`] as of `now`: the clock delivery ages,
    /// reservation expiry and new reservations are measured on.
    pub fn admit_against_forecast_at<T>(
        &self,
        now: DateTime<Utc>,
        decide: impl FnOnce(&HostLoadForecast) -> Result<ForecastDecision<T>, OrbitError>,
    ) -> Result<T, OrbitError> {
        let profiles = self.resource_profiles()?;
        let horizon = self
            .context
            .settings()
            .auto_concurrency()
            .horizon_cap_minutes;
        let store = self.sqlite_store()?;
        // Any wait for a CPU window happens here, outside the lock.
        self.host_load.prepare();
        let jobs = delivery_jobs();
        let query = AdmissionLoadQuery {
            now,
            delivery_jobs: &jobs,
            reservations_expire_before: now - Duration::minutes(RESERVATION_TTL_MINUTES),
        };
        let mut decide = Some(decide);
        let mut outcome = None;
        store.admit_under_host_lock(&query, &mut |load| {
            let decide = decide.take().ok_or_else(|| {
                OrbitError::Execution("the admission decision ran twice".to_string())
            })?;
            let forecast = build_forecast(
                load,
                Arc::clone(&profiles),
                self.host_load.measure(),
                horizon,
                now,
            );
            let decision = decide(&forecast)?;
            outcome = Some(decision.outcome);
            Ok(decision
                .admitted
                .into_iter()
                .map(|admitted| NewAdmissionReservation {
                    workspace_id: admitted.lookup.workspace_id,
                    task_id: admitted.task_id,
                    work_class: admitted.lookup.work_class,
                    complexity: admitted.lookup.complexity,
                    base_class: admitted.lookup.base_class.as_str().to_string(),
                })
                .collect())
        })?;
        outcome.ok_or_else(|| OrbitError::Execution("the admission decision never ran".to_string()))
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
