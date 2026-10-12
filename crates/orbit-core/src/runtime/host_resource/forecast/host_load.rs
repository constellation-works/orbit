//! The host's measured load now, for the forecast baseline.
//!
//! CPU is busy cores from `/proc/stat` tick deltas and memory is
//! `MemTotal - MemAvailable` bytes: the units the task samples use. It never
//! reads `/proc/loadavg`, which counts the run queue: a saturated host at load
//! 36 on 15 cores would otherwise show about 20 phantom cores of load that is
//! not from tasks.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::HostLoad;
use crate::runtime::host_resource::{HostUsageReading, read_host_usage};

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

    /// Make sure a recent reading at least [`MIN_CPU_WINDOW`] old exists,
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
                    MIN_CPU_WINDOW.saturating_sub(at.elapsed())
                }
                _ => {
                    *previous =
                        read_host_usage(&self.proc_root).map(|reading| (Instant::now(), reading));
                    if previous.is_some() {
                        MIN_CPU_WINDOW
                    } else {
                        Duration::ZERO
                    }
                }
            }
        };
        if !wait.is_zero() {
            std::thread::sleep(wait);
        }
    }

    /// The host's load now: CPU over the interval since the previous reading,
    /// memory as read. `None` when the host cannot be read; CPU alone is
    /// unknown when no ticks passed since the previous reading.
    pub(crate) fn measure(&self) -> Option<HostLoad> {
        let reading = read_host_usage(&self.proc_root)?;
        let mut previous = self
            .previous
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let cpu_cores = previous.as_ref().and_then(|(_, before)| {
            let total = reading.total_ticks.checked_sub(before.total_ticks)?;
            let busy = reading.busy_ticks.checked_sub(before.busy_ticks)?;
            (total > 0).then(|| busy as f64 / total as f64 * f64::from(reading.cpu_count))
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
