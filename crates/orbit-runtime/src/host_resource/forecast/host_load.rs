//! The host's measured load now, for the forecast baseline.
//!
//! CPU is busy cores from native tick deltas and memory uses the native
//! unavailable-memory accounting: the same reader as host-minute history.
//! CPU is reported twice: over the short interval since the previous reading,
//! and over the trailing minute or more since the newest kept reading at least
//! [`SUSTAINED_CPU_WINDOW`] old, which a short spike barely moves.
//! It never reads `/proc/loadavg`, which counts the run queue: a saturated host at load
//! 36 on 15 cores would otherwise show about 20 phantom cores of load that is
//! not from tasks.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use chrono::{DateTime, TimeDelta, Utc};

use super::{HostCapacity, HostLoad, SustainedCpu};
use crate::host_resource::{HostUsageReading, read_host_usage};

/// Shortest interval CPU utilization is measured over.
const MIN_CPU_WINDOW: Duration = Duration::from_millis(250);
/// A previous reading older than this no longer says what the host does now.
const MAX_CPU_WINDOW: Duration = Duration::from_secs(120);
/// Shortest interval the sustained CPU reading covers.
const SUSTAINED_CPU_WINDOW: TimeDelta = TimeDelta::seconds(60);
/// Longest interval the sustained CPU reading covers.
const MAX_SUSTAINED_CPU_WINDOW: TimeDelta = TimeDelta::seconds(120);
/// Readings closer together than this are not all kept for the sustained
/// window, which bounds how many are.
const SUSTAINED_READING_SPACING: TimeDelta = TimeDelta::seconds(5);

/// Reads the host's load. Utilization needs two tick readings, so the probe
/// keeps the previous one, and the readings of the last two minutes for the
/// sustained window; the load itself is computed fresh on every call.
pub(crate) struct HostLoadProbe {
    proc_root: PathBuf,
    previous: Mutex<Option<(Instant, HostUsageReading)>>,
    /// Readings `measure` took, oldest first, stamped with its `now`.
    kept: Mutex<VecDeque<(DateTime<Utc>, HostUsageReading)>>,
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
            kept: Mutex::new(VecDeque::new()),
        }
    }

    fn min_cpu_window(&self) -> Duration {
        #[cfg(target_os = "macos")]
        if self.proc_root == std::path::Path::new("/proc") {
            return crate::host_resource::platform::CPU_BASELINE_WINDOW;
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
                && let Some(fixture_wait) = crate::host_load_sampling_wait_override()
            {
                fixture_wait(wait);
                return;
            }
            std::thread::sleep(wait);
        }
    }

    /// The host's core count and memory, read now without a CPU window.
    /// `None` when the host cannot be read.
    pub(crate) fn capacity(&self) -> Option<HostCapacity> {
        read_host_usage(&self.proc_root).map(|reading| HostCapacity {
            cpu_count: reading.cpu_count,
            memory_total_bytes: reading.memory_total_bytes,
        })
    }

    /// The host's load as of `now`: CPU over the interval since the previous
    /// reading and over the sustained window, memory as read. `None` when the
    /// host cannot be read; CPU alone is unknown when no ticks passed since
    /// the previous reading.
    pub(crate) fn measure(&self, now: DateTime<Utc>) -> Option<HostLoad> {
        let mut previous = self
            .previous
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut kept = self
            .kept
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(reading) = read_host_usage(&self.proc_root) else {
            *previous = None;
            kept.clear();
            return None;
        };
        // A clock that stepped back makes every kept stamp meaningless.
        if kept.back().is_some_and(|(at, _)| *at > now) {
            kept.clear();
        }
        while kept
            .front()
            .is_some_and(|(at, _)| now - *at > MAX_SUSTAINED_CPU_WINDOW)
        {
            kept.pop_front();
        }
        let sustained = kept
            .iter()
            .rev()
            .find(|(at, _)| now - *at >= SUSTAINED_CPU_WINDOW)
            .and_then(|(at, before)| {
                let (busy, total) = reading.cpu_delta(*before)?;
                let cores = busy as f64 / total as f64 * f64::from(reading.cpu_count);
                Some(SustainedCpu { since: *at, cores })
            });
        // Only the newest reading at least a window old is still needed.
        while kept.len() > 1
            && kept
                .get(1)
                .is_some_and(|(at, _)| now - *at >= SUSTAINED_CPU_WINDOW)
        {
            kept.pop_front();
        }
        if kept
            .back()
            .is_none_or(|(at, _)| now - *at >= SUSTAINED_READING_SPACING)
        {
            kept.push_back((now, reading));
        }
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
            sustained_cpu: sustained,
            cpu_count: reading.cpu_count,
            memory_bytes: reading.memory_used_bytes,
            memory_total_bytes: reading.memory_total_bytes,
        })
    }
}
