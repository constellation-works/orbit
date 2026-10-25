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

use super::RuntimeKernel;
use crate::host_resource::accuracy::{AccuracySelection, ForecastAccuracy, forecast_accuracy};
use crate::host_resource::delivery::delivery_jobs;
use crate::host_resource::forecast::{
    AdmissionReservationRequest, BASELINE_HISTORY_MINUTES, ForecastDecision, HostCapacity,
    HostLoadForecast, HostLoadProbe, RESERVATION_TTL_MINUTES, build_forecast,
};
use crate::host_resource::profile::{ProfileBounds, ProfileLookup, ProfileMatch, ProfileSet};
use crate::host_resource::{
    HostResourceMonitor, HostResourceProbe, HostResourceStatus, ResourceAdmission,
};
use crate::host_signal::{HostSignalProbe, ScheduledShutdown};
use crate::task_pr_forge::TaskPrForge;

/// Days an execution claim in this workspace counts as evidence that the
/// claiming host serves it for creation-time crew draws. Fixed, not a
/// config key: a follower that stops pulling ages out after the window.
const SERVING_CLAIM_EVIDENCE_DAYS: i64 = 7;

impl RuntimeKernel {
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

    /// Carry `labels` as this host's `machine.labels`, already normalized. A
    /// fixture composes a labelled host the way it composes
    /// [`Self::with_host_os`]; production runtimes take the admitted setting.
    pub fn with_machine_labels(mut self, labels: Vec<String>) -> Self {
        self.context.set_machine_labels(labels);
        self
    }

    /// This host's admitted `machine.labels`, without the implicit OS label.
    pub fn machine_labels(&self) -> &[String] {
        self.context.settings().machine_labels()
    }

    /// The capability labels this host carries for a task's
    /// `requires_labels`: its `machine.labels` plus its OS as an implicit
    /// label. Local admission checks a task against this set, and a pull
    /// drain declares the configured part to its owner beside its OS.
    pub fn host_labels(&self) -> std::collections::BTreeSet<String> {
        orbit_types::task::effective_host_labels(self.machine_labels(), self.host_os)
    }

    /// Declare the registered remote hosts, by machine id, with the
    /// operating system recorded for each at registration. The composition
    /// layer reads them from the host registry; a fixture composes a fleet
    /// the same way it composes [`Self::with_host_os`]. Registration is not
    /// service: [`Self::serving_host_oses`] counts a listed host only when
    /// this workspace's claim records show it pulled here recently. This
    /// host's own OS always counts as serving and need not be listed.
    pub fn with_registered_host_oses(
        mut self,
        hosts: impl IntoIterator<Item = (String, orbit_types::task::HostOs)>,
    ) -> Self {
        self.registered_host_oses = hosts.into_iter().collect();
        self
    }

    /// The operating systems that count as serving this workspace for
    /// creation-time draws: this host's, when known, plus the recorded OS
    /// of each registered host with durable evidence, scoped to this
    /// workspace, that it pulled here recently — an execution claim in
    /// this workspace's own claim records within the last seven days.
    /// A registered host with no
    /// such claim (it serves only other workspaces, or none) never
    /// counts, and a follower that stops pulling ages out when its last
    /// claim leaves the window. Unreadable claim records leave only this
    /// host's OS, which is exactly the single-host draw behaviour.
    pub fn serving_host_oses(&self) -> std::collections::BTreeSet<orbit_types::task::HostOs> {
        let mut oses = std::collections::BTreeSet::new();
        oses.extend(self.host_os);
        if self.registered_host_oses.is_empty() {
            return oses;
        }
        let Ok(recent) = self.recently_claiming_machines() else {
            return oses;
        };
        for (machine_id, os) in &self.registered_host_oses {
            if recent.contains_key(machine_id) {
                oses.insert(*os);
            }
        }
        oses
    }

    /// Which registered hosts run one of `wanted`, and whether each is
    /// pulling here: `no registered host runs macos`, or `registered macos
    /// host(s): mac-mini pulling (claimed here within 7 days), mbp not
    /// pulling (no claim here within 7 days)`. Pulling is the same claim
    /// evidence [`Self::serving_host_oses`] counts, not a live heartbeat.
    /// Unreadable records leave the registered hosts' pulling status unknown.
    /// `None` when `wanted` is empty.
    pub fn host_os_fleet_note(
        &self,
        wanted: &std::collections::BTreeSet<orbit_types::task::HostOs>,
    ) -> Option<String> {
        if wanted.is_empty() {
            return None;
        }
        let names = wanted
            .iter()
            .map(|os| os.as_str())
            .collect::<Vec<_>>()
            .join(" or ");
        let matching = self
            .registered_host_oses
            .iter()
            .filter(|(_, os)| wanted.contains(os))
            .collect::<Vec<_>>();
        if matching.is_empty() {
            return Some(format!("no registered host runs {names}"));
        }
        let recent = self.recently_claiming_machines();
        let days = SERVING_CLAIM_EVIDENCE_DAYS;
        let hosts = matching
            .into_iter()
            .map(|(machine_id, _)| match &recent {
                Err(_) => format!("{machine_id} (claim records unreadable)"),
                Ok(recent) => match recent.get(machine_id) {
                    Some(name) => format!(
                        "{} pulling (claimed here within {days} days)",
                        name.as_deref().unwrap_or(machine_id)
                    ),
                    None => format!("{machine_id} not pulling (no claim here within {days} days)"),
                },
            })
            .collect::<Vec<_>>()
            .join(", ");
        Some(format!("registered {names} host(s): {hosts}"))
    }

    /// Machines with an execution claim in this workspace created or
    /// updated within [`SERVING_CLAIM_EVIDENCE_DAYS`], by machine id, with
    /// the name the claim recorded. Propagate unreadable records so callers
    /// can distinguish missing evidence from an unsuccessful inspection.
    fn recently_claiming_machines(
        &self,
    ) -> Result<std::collections::BTreeMap<String, Option<String>>, OrbitError> {
        let cutoff = Utc::now() - Duration::days(SERVING_CLAIM_EVIDENCE_DAYS);
        let claims = self.inspect_execution_claims()?;
        let mut recent = std::collections::BTreeMap::new();
        for claim in claims.iter().filter(|claim| {
            [&claim.updated_at, &claim.created_at]
                .into_iter()
                .filter_map(|stamp| DateTime::parse_from_rfc3339(stamp).ok())
                .any(|stamp| stamp.with_timezone(&Utc) >= cutoff)
        }) {
            let location = &claim.claim.executed_on;
            let name = recent
                .entry(location.machine_id.clone())
                .or_insert_with(|| None);
            if name.is_none() {
                name.clone_from(&location.machine_name);
            }
        }
        Ok(recent)
    }

    /// Read the browser kit admission checks `ui` tasks against from `home`
    /// rather than the current user's. A fixture composes a host with a kit,
    /// or without one, in a home it controls.
    pub fn with_browser_kit_home(mut self, home: &std::path::Path) -> Self {
        self.browser_kit_home = Some(home.to_path_buf());
        self
    }

    /// Whether this host's headless browser kit is verified now: installed,
    /// with the smoke test `orbit browser-kit verify` recorded against the
    /// Playwright version and browser revision it holds. Only then may a `ui`
    /// task start here, and only then does a pull drain declare the kit to
    /// its owner. Filesystem reads only, made on every call, so a kit
    /// installed mid-window counts at the next admission pass.
    pub fn browser_kit_verified(&self) -> bool {
        use orbit_exec::browser_kit::{KitInspection, KitLayout};
        let layout = match &self.browser_kit_home {
            Some(home) => KitLayout::for_home(home),
            None => match KitLayout::current_user() {
                Ok(layout) => layout,
                Err(_) => return false,
            },
        };
        layout.root().is_dir() && KitInspection::inspect(&layout).is_verified()
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

    pub fn task_pr_forge(&self) -> &dyn TaskPrForge {
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
            ProfileBounds::from(&self.auto_concurrency_settings()),
            Utc::now(),
        )
    }

    /// The host's core count and memory, as admission's forecast reads them:
    /// what `admit_cpu_percent` and `admit_memory_percent` are shares of.
    /// `None` when the host cannot be read.
    pub fn host_capacity(&self) -> Option<HostCapacity> {
        self.host_load.capacity()
    }

    /// The profile describing one delivery, and the fallback step that found
    /// it.
    pub fn resource_profile_for(&self, lookup: &ProfileLookup) -> Result<ProfileMatch, OrbitError> {
        Ok(self.resource_profiles()?.lookup(lookup))
    }

    /// Record one drain admission pass's forecast and decisions, for the
    /// forecast accuracy report.
    pub fn record_admission_forecast(
        &self,
        record: &AdmissionForecastRecord,
    ) -> Result<(), OrbitError> {
        self.sqlite_store()?.record_admission_forecast(record)
    }

    /// Read a selected cohort of recorded forecasts, bounded above by `report_at`.
    /// That same snapshot determines whether target minutes have fully ended.
    /// This does not reconcile, prune, sample, or change admission settings.
    /// The report's `limiters` are left empty: the drains' pass counters are
    /// joined by the application's `forecast_accuracy_selected`.
    pub fn forecast_accuracy_measured(
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
        let (minutes, deliveries) = match records.first() {
            Some(first) => {
                let lower = first.forecast_at - Duration::minutes(1);
                let jobs = delivery_jobs();
                (
                    store.host_resource_minutes(lower)?,
                    store.delivery_runs_overlapping(&jobs, lower, report_at)?,
                )
            }
            None => (Vec::new(), Vec::new()),
        };
        let covered_since = records
            .iter()
            .filter_map(|record| record.covered_deliveries.as_ref())
            .flatten()
            .map(|delivery| delivery.admitted_at)
            .min();
        let resources = covered_since
            .map(|since| store.delivery_run_resources(since))
            .transpose()?
            .unwrap_or_default();
        let report = forecast_accuracy(
            &records,
            &minutes,
            &deliveries,
            &resources,
            selection,
            report_at,
        );
        Ok(report)
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
    pub fn admit_against_forecast_replacing_at<T>(
        &self,
        now: DateTime<Utc>,
        remove: &[(&str, &str)],
        decide: impl FnOnce(&HostLoadForecast) -> Result<ForecastDecision<T>, OrbitError>,
    ) -> Result<T, OrbitError> {
        let profiles = self.resource_profiles()?;
        let horizon = self.auto_concurrency_settings().horizon_cap_minutes;
        let store = self.sqlite_store()?;
        // Any wait for a CPU window happens here, outside the lock.
        self.host_load.prepare();
        let jobs = delivery_jobs();
        let query = AdmissionLoadQuery {
            now,
            delivery_jobs: &jobs,
            reservations_expire_before: now - Duration::minutes(RESERVATION_TTL_MINUTES),
            resource_minutes_since: Some(now - Duration::minutes(BASELINE_HISTORY_MINUTES)),
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
                self.host_load.measure(now),
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
    pub fn release_admission_reservations(
        &self,
        remove: &[(&str, &str)],
    ) -> Result<(), OrbitError> {
        let now = Utc::now();
        self.sqlite_store()?.admit_under_host_lock_replacing(
            &AdmissionLoadQuery {
                now,
                delivery_jobs: &[],
                reservations_expire_before: now - Duration::minutes(RESERVATION_TTL_MINUTES),
                resource_minutes_since: None,
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
