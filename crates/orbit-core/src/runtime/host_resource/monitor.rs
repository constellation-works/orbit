//! Shared sampling, bounded pressure history and admission reporting.

use chrono::Utc;
use orbit_common::fs::io::{
    FileLockOptions, atomic_write_text_volatile, open_read_only_no_follow,
    with_exclusive_file_lock_options,
};
use orbit_config::ResourceThrottleSettings;
use serde::{Deserialize, Serialize};
use std::io::{self, Read};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant};

use super::{
    HostResourceProbe, HostResourceStatus, RESOURCE_TICK_INTERVAL, ResourceAdmission,
    ResourcePressureEvaluator,
};

/// Background sampling stops once admission has not been consulted this long.
const RESOURCE_TICK_IDLE: Duration = Duration::from_secs(300);

/// Shared probe and evaluator for one runtime or serving host.
pub struct HostResourceMonitor {
    probe: Arc<dyn HostResourceProbe>,
    settings: ResourceThrottleSettings,
    evaluator: Mutex<ResourcePressureEvaluator>,
    history_path: Option<PathBuf>,
    /// Whether admission checks keep the evaluator fed between drain passes.
    /// Off for injected probes, whose fixtures drive every sample themselves.
    background: bool,
    /// Last admission consult, read by the background sampler to retire.
    demand: Mutex<Option<Instant>>,
    sampling: AtomicBool,
    /// The last reported admission, so each transition is logged once.
    reported: Mutex<ResourceAdmission>,
}

impl HostResourceMonitor {
    pub fn new(probe: Arc<dyn HostResourceProbe>, settings: ResourceThrottleSettings) -> Self {
        Self {
            probe,
            settings,
            evaluator: Mutex::new(ResourcePressureEvaluator::default()),
            history_path: None,
            background: false,
            demand: Mutex::new(None),
            sampling: AtomicBool::new(false),
            reported: Mutex::new(ResourceAdmission::default()),
        }
    }

    /// Share bounded, freshness-checked hysteresis across processes on this host.
    pub(crate) fn with_shared_history(mut self, global_root: &std::path::Path) -> Self {
        self.history_path = Some(global_root.join("cache/host-resource-pressure.json"));
        self
    }

    /// Keep sampling in the background while admission is being consulted.
    #[must_use]
    pub fn sampled_in_background(mut self) -> Self {
        self.background = true;
        self
    }

    /// The admission verdict for `paths` [ORB-13901]. Disabled settings return
    /// an empty verdict without sampling. Throttle and unknown-telemetry
    /// transitions are logged once each.
    pub fn admission(self: &Arc<Self>, paths: &[PathBuf]) -> ResourceAdmission {
        if !self.settings.enabled {
            return ResourceAdmission::default();
        }
        self.keep_sampling(paths);
        let admission = ResourceAdmission::from_status(&self.snapshot(paths));
        self.report_transition(&admission);
        admission
    }

    fn report_transition(&self, admission: &ResourceAdmission) {
        let mut reported = self.reported.lock().unwrap_or_else(PoisonError::into_inner);
        match (&reported.throttle, &admission.throttle) {
            (None, Some(throttle)) => tracing::warn!(
                target: "orbit.core.host_resource",
                "admissions throttled: {}",
                throttle.describe()
            ),
            (Some(_), None) => tracing::info!(
                target: "orbit.core.host_resource",
                "admissions resumed: host resources are back below their resume marks"
            ),
            _ => {}
        }
        if reported.unknown.is_empty() && !admission.unknown.is_empty() {
            tracing::warn!(
                target: "orbit.core.host_resource",
                "resource telemetry unknown ({}); admission fails open",
                admission.unknown.join(", ")
            );
        } else if !reported.unknown.is_empty() && admission.unknown.is_empty() {
            tracing::info!(
                target: "orbit.core.host_resource",
                "resource telemetry available again"
            );
        }
        *reported = admission.clone();
    }

    fn keep_sampling(self: &Arc<Self>, paths: &[PathBuf]) {
        if !self.background {
            return;
        }
        *self.demand.lock().unwrap_or_else(PoisonError::into_inner) = Some(Instant::now());
        if self.sampling.swap(true, Ordering::AcqRel) {
            return;
        }
        let monitor: Weak<Self> = Arc::downgrade(self);
        let paths = paths.to_vec();
        let spawned = std::thread::Builder::new()
            .name("orbit-host-resource".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(RESOURCE_TICK_INTERVAL);
                    let Some(monitor) = monitor.upgrade() else {
                        return;
                    };
                    let idle = monitor
                        .demand
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .is_none_or(|at| at.elapsed() > RESOURCE_TICK_IDLE);
                    if idle {
                        monitor.sampling.store(false, Ordering::Release);
                        return;
                    }
                    monitor.snapshot(&paths);
                }
            });
        if let Err(error) = spawned {
            self.sampling.store(false, Ordering::Release);
            tracing::warn!(
                target: "orbit.core.host_resource",
                %error,
                "background resource sampling unavailable; pressure is evaluated per admission pass"
            );
        }
    }

    pub fn snapshot(&self, paths: &[PathBuf]) -> HostResourceStatus {
        // Serialize sampling and evaluation together: a delayed reader cannot apply an
        // older sample after a newer sample and reset its hysteresis.
        let mut evaluator = self
            .evaluator
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if self.settings.enabled
            && let Some(path) = &self.history_path
        {
            // Serialize read/sample/evaluate/write across processes too. The bounded
            // lock prevents resource telemetry from stalling a dispatch indefinitely.
            let result = with_exclusive_file_lock_options(
                path,
                "host resource pressure",
                FileLockOptions {
                    timeout: Duration::from_millis(100),
                    warn_after: Duration::from_millis(100),
                },
                || -> io::Result<HostResourceStatus> {
                    match read_history(path) {
                        Ok(history) => *evaluator = history,
                        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                        Err(error) => {
                            tracing::debug!(%error, "shared resource history unavailable");
                        }
                    }
                    let status =
                        evaluator.evaluate(self.probe.sample(paths), &self.settings, Utc::now());
                    let history = PressureHistory {
                        schema_version: 1,
                        evaluator: &evaluator,
                    };
                    let encoded = serde_json::to_string(&history).map_err(io::Error::other)?;
                    if let Err(error) = atomic_write_text_volatile(path, &encoded) {
                        tracing::debug!(%error, "shared resource history could not be saved");
                    }
                    Ok(status)
                },
            );
            match result {
                Ok(status) => return status,
                Err(error) => {
                    tracing::debug!(%error, "using local resource history");
                }
            }
        }
        evaluator.evaluate(self.probe.sample(paths), &self.settings, Utc::now())
    }
}

#[derive(Serialize)]
struct PressureHistory<'a> {
    schema_version: u8,
    evaluator: &'a ResourcePressureEvaluator,
}

fn read_history(path: &std::path::Path) -> io::Result<ResourcePressureEvaluator> {
    #[derive(Deserialize)]
    struct History {
        schema_version: u8,
        evaluator: ResourcePressureEvaluator,
    }
    let file = open_read_only_no_follow(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("resource history is not a regular file"));
    }
    let mut encoded = String::new();
    file.take(64 * 1024 + 1).read_to_string(&mut encoded)?;
    if encoded.len() > 64 * 1024 {
        return Err(io::Error::other("resource history exceeds size limit"));
    }
    let history: History = serde_json::from_str(&encoded).map_err(io::Error::other)?;
    if history.schema_version != 1 {
        return Err(io::Error::other("unsupported resource history version"));
    }
    Ok(history.evaluator)
}
