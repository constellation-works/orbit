//! Cooperative build admission settings and per-invocation wait snapshots.

use std::io::Read;
use std::path::{Path, PathBuf};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::OrbitError;

/// Invocation-private directory where build wrappers heartbeat admission waits.
pub const WAIT_DIRECTORY_ENV: &str = "ORBIT_ACTIVITY_BUILD_BUDGET_DIR";

/// Wait statistics. Overlapping commands contribute separately to totals,
/// while `queued_wall_ms` counts their interval union for deadline credit.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildBudgetWaits {
    pub count: u64,
    pub total_ms: u64,
    pub longest_ms: u64,
    pub queued_wall_ms: u64,
    pub deadline_extension_ms: u64,
}

#[derive(Deserialize)]
struct WaitSnapshot {
    started_monotonic_ms: u64,
    elapsed_ms: u64,
    #[serde(default)]
    finished: bool,
}

// Match the wrapper's cross-process clock without using civil time.
#[cfg(unix)]
fn monotonic_ms() -> Option<u64> {
    let mut timestamp = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock_gettime writes one timespec through this valid exclusive
    // pointer. CLOCK_MONOTONIC requires no other resources.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut timestamp) } != 0 {
        return None;
    }
    let seconds = u64::try_from(timestamp.tv_sec).ok()?;
    let nanos = u64::try_from(timestamp.tv_nsec).ok()?;
    Some(
        seconds
            .saturating_mul(1000)
            .saturating_add(nanos / 1_000_000),
    )
}

#[cfg(not(unix))]
fn monotonic_ms() -> Option<u64> {
    None
}

/// Read bounded, atomic snapshots; missing or invalid telemetry grants no credit.
/// The wrapper measures elapsed time monotonically and stops updating when killed.
pub fn read_waits(directory: &Path, timeout_ms: u64) -> BuildBudgetWaits {
    let mut stats = BuildBudgetWaits::default();
    let Ok(entries) = std::fs::read_dir(directory) else {
        return stats;
    };
    let mut intervals = Vec::new();
    for entry in entries.take(4096).flatten() {
        if entry.path().extension().is_none_or(|ext| ext != "json")
            || !entry.file_type().is_ok_and(|kind| kind.is_file())
        {
            continue;
        }
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        let Ok(file) = options.open(entry.path()) else {
            continue;
        };
        if !file.metadata().is_ok_and(|meta| meta.is_file()) {
            continue;
        }
        let mut bytes = Vec::new();
        if file.take(1024).read_to_end(&mut bytes).is_err() {
            continue;
        }
        let Ok(snapshot) = serde_json::from_slice::<WaitSnapshot>(&bytes) else {
            continue;
        };
        let mut elapsed_ms = snapshot.elapsed_ms;
        if !snapshot.finished {
            // A killed wrapper releases this lock, freezing credit at its
            // heartbeat. For a live wait, credit up to the current instant,
            // including admission begun just before the original deadline.
            if let Ok(lock) = options.open(entry.path().with_extension("lock"))
                && lock.metadata().is_ok_and(|meta| meta.is_file())
                && lock
                    .try_lock_exclusive()
                    .is_err_and(|error| error.kind() == std::io::ErrorKind::WouldBlock)
                && let Some(now) = monotonic_ms()
            {
                elapsed_ms = elapsed_ms.max(now.saturating_sub(snapshot.started_monotonic_ms));
            }
        }
        stats.count += 1;
        stats.total_ms = stats.total_ms.saturating_add(elapsed_ms);
        stats.longest_ms = stats.longest_ms.max(elapsed_ms);
        intervals.push((
            snapshot.started_monotonic_ms,
            snapshot.started_monotonic_ms.saturating_add(elapsed_ms),
        ));
    }
    intervals.sort_unstable();
    let mut end = 0;
    for (start, next_end) in intervals {
        stats.queued_wall_ms = stats
            .queued_wall_ms
            .saturating_add(next_end.saturating_sub(start.max(end)));
        end = end.max(next_end);
    }
    stats.deadline_extension_ms = stats.queued_wall_ms.min(timeout_ms);
    stats
}

/// Resolved host slot capacity and the operator's settings path.
pub struct BuildBudgetCapacity {
    pub slots: u32,
    pub enabled: bool,
    pub settings_file: PathBuf,
}

impl BuildBudgetCapacity {
    /// Read the same environment/file/default precedence as build-budget.py.
    pub fn read() -> Result<Self, OrbitError> {
        let directory = std::env::var_os("ORBIT_BUILD_BUDGET_DIR")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .map(|path| {
                if let Ok(rest) = path.strip_prefix("~/") {
                    crate::fs::path::home_dir().map(|home| home.join(rest))
                } else {
                    Ok(path)
                }
            })
            .transpose()?
            .unwrap_or(crate::fs::path::home_dir()?.join(".orbit/cache/build-budget"));
        if directory.is_symlink() {
            return Err(OrbitError::InvalidInput(format!(
                "build-budget directory is a symbolic link: {}",
                directory.display()
            )));
        }
        let settings_file = directory.join("slots");
        let raw = match std::env::var("ORBIT_BUILD_SLOTS") {
            Ok(value) => value,
            Err(std::env::VarError::NotPresent) => match std::fs::read_to_string(&settings_file) {
                Ok(value) => value.trim().to_string(),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => "2".to_string(),
                Err(error) => {
                    return Err(OrbitError::Io(format!(
                        "read {}: {error}",
                        settings_file.display()
                    )));
                }
            },
            Err(error) => {
                return Err(OrbitError::InvalidInput(format!(
                    "ORBIT_BUILD_SLOTS: {error}"
                )));
            }
        };
        let slots = raw
            .parse::<u32>()
            .ok()
            .filter(|slots| (1..=128).contains(slots));
        let slots = slots
            .filter(|_| !raw.starts_with('0') && raw.bytes().all(|b| b.is_ascii_digit()))
            .ok_or_else(|| {
                OrbitError::InvalidInput(format!(
                    "ORBIT_BUILD_SLOTS must be a decimal integer from 1 through 128; got {raw:?}"
                ))
            })?;
        let enabled = match std::env::var("ORBIT_BUILD_BUDGET").as_deref() {
            Ok("0") => false,
            Ok("1") | Err(std::env::VarError::NotPresent) => true,
            _ => {
                return Err(OrbitError::InvalidInput(
                    "ORBIT_BUILD_BUDGET must be 0 or 1".into(),
                ));
            }
        };
        Ok(Self {
            slots,
            enabled,
            settings_file,
        })
    }

    /// Capacity warning with both counts and the concrete remedy.
    pub fn warning(&self, concurrency: u64) -> Option<String> {
        (self.enabled && concurrency > u64::from(self.slots)).then(|| format!(
            "Drain concurrency {concurrency} exceeds host build slots {}. Change ORBIT_BUILD_SLOTS or {} to adjust slots, or lower drain concurrency with orbit run concurrency.",
            self.slots, self.settings_file.display()
        ))
    }
}
