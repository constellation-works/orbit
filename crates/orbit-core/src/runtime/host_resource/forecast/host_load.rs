//! The host's load for the forecast baseline: its newest completed minute
//! with that minute's task load taken out, and its measured load now.
//!
//! CPU is busy cores from native tick deltas and memory uses the native
//! unavailable-memory accounting: the same reader as host-minute history.
//! It never reads `/proc/loadavg`, which counts the run queue: a saturated host at load
//! 36 on 15 cores would otherwise show about 20 phantom cores of load that is
//! not from tasks.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono::{DateTime, TimeDelta, Utc};
use orbit_store::contracts::ResourceUsageStoreBackend;

use super::HostLoad;
use crate::runtime::host_resource::{HostUsageReading, read_host_usage};

const MINUTE: TimeDelta = TimeDelta::minutes(1);
/// How long after a host minute starts every run minute overlapping it is
/// recorded: a run minute that started before the host minute ended ends
/// within the next minute, and its worker, sampling every 5 s, writes it at
/// its first sample after that.
const TASK_MINUTES_SETTLED: TimeDelta = TimeDelta::seconds(2 * 60 + 10);
/// A host minute that started longer ago than this no longer says what the
/// host does now.
const MAX_MINUTE_AGE: TimeDelta = TimeDelta::minutes(5);

/// The host's load outside tasks over one completed wall-clock minute.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct NonTaskMinute {
    pub(crate) minute_at: DateTime<Utc>,
    pub(crate) cpu_cores: f64,
    pub(crate) memory_bytes: f64,
}

/// The newest recorded host minute whose run minutes have all been written,
/// minus those run minutes, each weighted by how much of the host minute it
/// covers. Both sides cover the same wall-clock window (CPU as means, memory
/// as peaks, as the workers record them), so a momentary spike, or a delivery
/// that started after the minute, adds
/// nothing. `None` when no host minute is recent enough or the store cannot
/// be read: the baseline then comes from the reading now.
pub(crate) fn non_task_minute(
    store: &dyn ResourceUsageStoreBackend,
    now: DateTime<Utc>,
) -> Option<NonTaskMinute> {
    let settled = now - TASK_MINUTES_SETTLED;
    let host = store
        .host_resource_minutes(now - MAX_MINUTE_AGE)
        .inspect_err(|error| {
            tracing::debug!(target: "orbit.core.host_resource", error = %error, "could not read host minutes");
        })
        .ok()?
        .into_iter()
        .rfind(|minute| minute.minute_at <= settled)?;
    let end = host.minute_at + MINUTE;
    let tasks = store
        .run_resource_minutes_started_between(host.minute_at - MINUTE, end)
        .inspect_err(|error| {
            tracing::debug!(target: "orbit.core.host_resource", error = %error, "could not read run minutes");
        })
        .ok()?;
    let (task_cpu, task_memory) = tasks.iter().fold((0.0, 0.0), |(cpu, memory), minute| {
        let overlap = (minute.minute_started_at + MINUTE).min(end)
            - minute.minute_started_at.max(host.minute_at);
        let share = overlap.num_milliseconds().max(0) as f64 / MINUTE.num_milliseconds() as f64;
        (
            cpu + minute.cpu_cores * share,
            memory + minute.memory_bytes as f64 * share,
        )
    });
    Some(NonTaskMinute {
        minute_at: host.minute_at,
        cpu_cores: (host.cpu_cores - task_cpu).max(0.0),
        memory_bytes: (host.memory_bytes as f64 - task_memory).max(0.0),
    })
}

/// Shortest interval CPU utilization is measured over.
const MIN_CPU_WINDOW: Duration = Duration::from_millis(250);
/// A previous reading older than this no longer says what the host does now.
const MAX_CPU_WINDOW: Duration = Duration::from_secs(120);

/// Reads the host's load. Utilization needs two tick readings, so the probe
/// keeps the previous one; the load itself is computed fresh on every call.
pub(crate) struct HostLoadProbe {
    proc_root: PathBuf,
    previous: Mutex<Option<(Instant, HostUsageReading)>>,
}

impl Default for HostLoadProbe {
    fn default() -> Self {
        Self::new(PathBuf::from("/proc"))
    }
}

impl HostLoadProbe {
    pub(crate) fn new(proc_root: PathBuf) -> Self {
        Self {
            proc_root,
            previous: Mutex::new(None),
        }
    }

    fn min_cpu_window(&self) -> Duration {
        #[cfg(target_os = "macos")]
        if self.proc_root == std::path::Path::new("/proc") {
            return crate::runtime::host_resource::platform::CPU_BASELINE_WINDOW;
        }
        MIN_CPU_WINDOW
    }

    /// Make sure a recent reading at least one native CPU window old exists,
    /// waiting for it if not. Called before the host admission lock, so the
    /// wait never holds it.
    pub(crate) fn prepare(&self) {
        let wait = {
            let mut previous = self
                .previous
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match previous.as_ref() {
                Some((at, _)) if at.elapsed() <= MAX_CPU_WINDOW => {
                    self.min_cpu_window().saturating_sub(at.elapsed())
                }
                _ => {
                    *previous =
                        read_host_usage(&self.proc_root).map(|reading| (Instant::now(), reading));
                    if previous.is_some() {
                        self.min_cpu_window()
                    } else {
                        Duration::ZERO
                    }
                }
            }
        };
        if !wait.is_zero() {
            #[cfg(feature = "test-support")]
            if self.proc_root != std::path::Path::new("/proc")
                && let Some(fixture_wait) = crate::runtime::host_load_sampling_wait_override()
            {
                fixture_wait(wait);
                return;
            }
            std::thread::sleep(wait);
        }
    }

    /// The host's load now: CPU over the interval since the previous reading,
    /// memory as read. `None` when the host cannot be read; CPU alone is
    /// unknown when no ticks passed since the previous reading.
    pub(crate) fn measure(&self) -> Option<HostLoad> {
        let mut previous = self
            .previous
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(reading) = read_host_usage(&self.proc_root) else {
            *previous = None;
            return None;
        };
        let cpu_cores = previous.as_ref().and_then(|(at, before)| {
            if at.elapsed() > MAX_CPU_WINDOW {
                return None;
            }
            let (busy, total) = reading.cpu_delta(*before)?;
            Some(busy as f64 / total as f64 * f64::from(reading.cpu_count))
        });
        *previous = Some((Instant::now(), reading));
        Some(HostLoad {
            cpu_cores,
            cpu_count: reading.cpu_count,
            memory_bytes: reading.memory_used_bytes,
            memory_total_bytes: reading.memory_total_bytes,
        })
    }
}
