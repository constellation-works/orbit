//! Host lifecycle, forge, and resource probes.

use std::path::PathBuf;
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use orbit_common::OrbitError;
use orbit_store::contracts::{
    AdmissionForecastRecord, AdmissionLoadQuery, AdmissionLoadStoreBackend,
    NewAdmissionReservation, ResourceUsageStoreBackend,
};
use orbit_types::task::NO_DIFF_EXPECTED_TAG;

use super::OrbitRuntime;
use crate::runtime::host_resource::accuracy::{
    AccuracySelection, ForecastAccuracy, forecast_accuracy,
};
use crate::runtime::host_resource::delivery::delivery_jobs;
use crate::runtime::host_resource::forecast::{
    AdmissionReservationRequest, ForecastDecision, HostLoadForecast, HostLoadProbe,
    RESERVATION_TTL_MINUTES, build_forecast, non_task_minute,
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

    /// Record one drain admission pass's forecast and decisions, for the
    /// forecast accuracy report.
    pub(crate) fn record_admission_forecast(
        &self,
        record: &AdmissionForecastRecord,
    ) -> Result<(), OrbitError> {
        self.sqlite_store()?.record_admission_forecast(record)
    }

    /// How the admission forecasts recorded on this host over the last
    /// 30 days compare with its measured minutes, and how
    /// each drain's shadow decisions compare with what it admitted.
    pub fn forecast_accuracy(&self) -> Result<ForecastAccuracy, OrbitError> {
        let report_at = Utc::now();
        self.forecast_accuracy_selected(AccuracySelection::last_30_days(report_at), report_at)
    }

    /// Read a selected cohort of recorded forecasts, bounded above by `report_at`.
    /// That same snapshot determines whether target minutes have fully ended.
    /// This does not reconcile, prune, sample, or change admission settings.
    pub fn forecast_accuracy_selected(
        &self,
        selection: AccuracySelection,
        report_at: DateTime<Utc>,
    ) -> Result<ForecastAccuracy, OrbitError> {
        if selection.since > report_at {
            return Err(OrbitError::InvalidInput(
                "accuracy since must not be after report time".into(),
            ));
        }
        if selection
            .drain
            .as_ref()
            .is_some_and(|id| id.trim().is_empty())
        {
            return Err(OrbitError::InvalidInput(
                "accuracy drain must not be empty".into(),
            ));
        }
        if selection
            .mode
            .as_deref()
            .is_some_and(|mode| !matches!(mode, "shadow" | "enforce"))
        {
            return Err(OrbitError::InvalidInput(
                "accuracy mode must be shadow or enforce".into(),
            ));
        }
        let store = self.sqlite_store()?;
        let mut records = store.admission_forecasts(selection.since)?;
        records.retain(|record| {
            record.forecast_at <= report_at
                && selection
                    .drain
                    .as_ref()
                    .is_none_or(|run| run == &record.run_id)
                && selection
                    .mode
                    .as_ref()
                    .is_none_or(|mode| mode == &record.mode)
        });
        let minutes = match records.first() {
            Some(first) => store.host_resource_minutes(first.forecast_at - Duration::minutes(1))?,
            None => Vec::new(),
        };
        Ok(forecast_accuracy(&records, &minutes, selection, report_at))
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
        self.admit_against_forecast_replacing_at(now, &[], decide)
    }

    /// Exchange reservation keys while computing the decision under one host
    /// lock. Returning an error preserves the old reservations.
    pub(crate) fn admit_against_forecast_replacing_at<T>(
        &self,
        now: DateTime<Utc>,
        remove: &[(&str, &str)],
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
        let minute = non_task_minute(&store, now);
        let jobs = delivery_jobs();
        let query = AdmissionLoadQuery {
            now,
            delivery_jobs: &jobs,
            reservations_expire_before: now - Duration::minutes(RESERVATION_TTL_MINUTES),
        };
        let mut decide = Some(decide);
        let mut outcome = None;
        store.admit_under_host_lock_replacing(&query, remove, &mut |load| {
            let decide = decide.take().ok_or_else(|| {
                OrbitError::Execution("the admission decision ran twice".to_string())
            })?;
            let forecast = build_forecast(
                load,
                Arc::clone(&profiles),
                self.host_load.measure(),
                minute,
                horizon,
                now,
            );
            let decision = decide(&forecast)?;
            outcome = Some(decision.outcome);
            Ok(decision.admitted.into_iter().map(new_reservation).collect())
        })?;
        outcome.ok_or_else(|| OrbitError::Execution("the admission decision never ran".to_string()))
    }

    /// Delete capacity under the host lock without sampling load or profiles.
    pub(crate) fn release_admission_reservations(
        &self,
        remove: &[(&str, &str)],
    ) -> Result<(), OrbitError> {
        let now = Utc::now();
        self.sqlite_store()?.admit_under_host_lock_replacing(
            &AdmissionLoadQuery {
                now,
                delivery_jobs: &[],
                reservations_expire_before: now - Duration::minutes(RESERVATION_TTL_MINUTES),
            },
            remove,
            &mut |_| Ok(Vec::new()),
        )?;
        Ok(())
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

fn new_reservation(admitted: AdmissionReservationRequest) -> NewAdmissionReservation {
    NewAdmissionReservation {
        workspace_id: admitted.lookup.workspace_id,
        task_id: admitted.task_id,
        work_class: admitted.lookup.work_class,
        complexity: admitted.lookup.complexity,
        base_class: admitted.lookup.base_class.as_str().to_string(),
    }
}
