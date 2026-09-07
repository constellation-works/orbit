//! Bounded, diagnostic advisory file locking shared by the SQLite ID allocator
//! and file-backed stores. [ORB-00412]
//!
//! Record mutation serializes through `fs2`
//! advisory (`flock(2)`) locks. A bare [`fs2::FileExt::lock_exclusive`] blocks
//! *indefinitely*: if a holder hangs — as opposed to crashing, since the OS
//! releases advisory locks on process death — every other agent stalls forever
//! with no signal about who holds the lock or why. Under concurrent
//! multi-agent use (parallel worktrees, unattended
//! ship-sweep) that silent stall is a whole-workspace availability risk.
//!
//! This module bounds the wait with a configurable deadline, records holder
//! metadata (PID + acquisition timestamp) into the lock file for diagnostics,
//! and emits a `tracing::warn!` once a waiter has been blocked past a
//! threshold. Holder metadata is advisory only — the OS `flock` is the source
//! of truth; the metadata is read solely to enrich the error/warning a blocked
//! waiter reports. A clean guard drop clears the metadata while still holding
//! the flock, while an abrupt process death leaves it behind as a crash signal.
//!
//! The returned [`FileLockGuard`] follows the RAII-guard pattern
//! (`docs/design-patterns/raii_guard.md`): the OS lock is released when the
//! guard is dropped, including on `?`-return and unwind.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use fs2::FileExt;
use orbit_common::OrbitError;
use serde::{Deserialize, Serialize};

/// Default upper bound on how long an exclusive acquisition blocks before
/// failing with a typed error. Deliberately generous: contention is normal
/// under concurrent multi-agent use, but a hung holder should not stall the
/// workspace forever.
pub(crate) const DEFAULT_LOCK_TIMEOUT: Duration = Duration::from_secs(30);

/// Emit a diagnostic warning once a waiter has been blocked at least this long.
const DEFAULT_WARN_AFTER: Duration = Duration::from_secs(3);

/// Poll interval for the try-lock retry loop.
const RETRY_INTERVAL: Duration = Duration::from_millis(50);

/// Tunables for a single lock acquisition. Use [`LockOptions::default`] for
/// production defaults; tests override the durations to stay fast and
/// deterministic.
#[derive(Debug, Clone, Copy)]
pub(crate) struct LockOptions {
    /// Fail the acquisition once the waiter has blocked this long.
    pub(crate) timeout: Duration,
    /// Warn (once) once the waiter has blocked this long.
    pub(crate) warn_after: Duration,
}

impl Default for LockOptions {
    fn default() -> Self {
        Self {
            timeout: DEFAULT_LOCK_TIMEOUT,
            warn_after: DEFAULT_WARN_AFTER,
        }
    }
}

/// RAII guard over an exclusive advisory file lock. A clean drop clears the
/// diagnostic holder metadata before the wrapped [`File`] is dropped (and
/// `fs2` unlocks on close), while a process crash leaves the metadata behind
/// for `orbit doctor` to report.
#[derive(Debug)]
#[must_use = "the advisory lock is released as soon as the guard is dropped"]
pub(crate) struct FileLockGuard {
    // Held purely for its Drop side effect: closing the file releases the
    // `flock`. Named with a leading underscore so dead-code analysis does not
    // flag the never-read field.
    _file: File,
}

impl Drop for FileLockGuard {
    fn drop(&mut self) {
        // Clear the diagnostic marker before closing the descriptor. The flock
        // is therefore held throughout the metadata transition, so a waiter
        // cannot observe a clean release as a stale holder or race a path
        // replacement. Best-effort matches acquisition metadata handling:
        // advisory diagnostics must never turn a successful release into an
        // application failure.
        let _ = clear_holder(&self._file);
    }
}

/// Metadata written into a lock file by its current holder. Advisory and
/// diagnostic only — never load-bearing for correctness.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct LockHolder {
    pid: u32,
    /// RFC 3339 acquisition timestamp.
    acquired_at: String,
    /// Human label of the operation holding the lock.
    label: String,
}

/// Public, read-only view of the holder metadata recorded in a lock file.
/// Advisory and diagnostic only (the OS `flock` is the source of truth, and
/// the OS releases advisory locks on process death) — consumers such as
/// `orbit doctor` use it to spot leftover metadata from crashed holders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockHolderInfo {
    /// PID recorded by the holder at acquisition time.
    pub pid: u32,
    /// RFC 3339 acquisition timestamp.
    pub acquired_at: String,
    /// Human label of the operation that acquired the lock.
    pub label: String,
}

/// Read the holder metadata recorded in a lock file, if any. Lenient like
/// [`read_holder`]: missing file, torn write, or legacy empty lock files
/// yield `None` rather than an error.
pub fn read_lock_holder(path: &Path) -> Option<LockHolderInfo> {
    read_holder(path).map(|holder| LockHolderInfo {
        pid: holder.pid,
        acquired_at: holder.acquired_at,
        label: holder.label,
    })
}

/// Acquire an exclusive advisory lock on `path` using production defaults,
/// creating the file (and parent directories) if needed. `label` names the
/// operation for diagnostics (e.g. `"id allocation"`).
pub(crate) fn acquire_exclusive(path: &Path, label: &str) -> Result<FileLockGuard, OrbitError> {
    acquire_exclusive_with(path, label, LockOptions::default())
}

/// Single-shot exclusive acquisition: returns `Ok(None)` immediately when
/// another process holds the lock instead of waiting. For callers whose
/// correct response to contention is "someone else is already doing this
/// pass, exit" (e.g. the routine sweep) rather than queueing behind the
/// holder. The OS releases the lock on process death, so a crashed holder
/// never wedges future acquisitions.
pub(crate) fn try_acquire_exclusive(
    path: &Path,
    label: &str,
) -> Result<Option<FileLockGuard>, OrbitError> {
    let parent = path.parent().ok_or_else(|| {
        OrbitError::Store(format!(
            "cannot determine lock parent for '{}'",
            path.display()
        ))
    })?;
    fs::create_dir_all(parent).map_err(|error| OrbitError::Io(error.to_string()))?;

    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|error| OrbitError::Io(error.to_string()))?;

    match file.try_lock_exclusive() {
        Ok(()) => {
            write_holder(&file, label);
            Ok(Some(FileLockGuard { _file: file }))
        }
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
        Err(error) => Err(OrbitError::Store(format!(
            "failed to acquire {label} lock '{}': {error}",
            path.display()
        ))),
    }
}

/// Acquire an exclusive advisory lock with explicit [`LockOptions`]. On timeout
/// returns [`OrbitError::Store`] whose message names the lock path and any
/// holder metadata, so a stalled waiter is observable rather than silent.
pub(crate) fn acquire_exclusive_with(
    path: &Path,
    label: &str,
    options: LockOptions,
) -> Result<FileLockGuard, OrbitError> {
    let parent = path.parent().ok_or_else(|| {
        OrbitError::Store(format!(
            "cannot determine lock parent for '{}'",
            path.display()
        ))
    })?;
    fs::create_dir_all(parent).map_err(|error| OrbitError::Io(error.to_string()))?;

    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|error| OrbitError::Io(error.to_string()))?;

    let started = Instant::now();
    let mut warned = false;
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => {
                // Best-effort holder metadata; advisory/diagnostic only.
                write_holder(&file, label);
                return Ok(FileLockGuard { _file: file });
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                let elapsed = started.elapsed();
                if elapsed >= options.timeout {
                    return Err(OrbitError::Store(format!(
                        "timed out after {:?} acquiring {label} lock '{}'{}",
                        options.timeout,
                        path.display(),
                        holder_suffix(path),
                    )));
                }
                if !warned && elapsed >= options.warn_after {
                    warned = true;
                    orbit_common::tracing::warn!(
                        target: "orbit.store.file_lock",
                        lock_path = %path.display(),
                        label,
                        waited_ms = elapsed.as_millis() as u64,
                        holder = %holder_description(path),
                        "still waiting for advisory file lock; a holder may be hung",
                    );
                }
                std::thread::sleep(RETRY_INTERVAL);
            }
            Err(error) => {
                return Err(OrbitError::Store(format!(
                    "failed to acquire {label} lock '{}': {error}",
                    path.display()
                )));
            }
        }
    }
}

/// Overwrite the lock file with the current holder's metadata. Best-effort: a
/// failure here never fails the acquisition, and a concurrent reader tolerating
/// a torn write is expected (see [`read_holder`]).
fn write_holder(file: &File, label: &str) {
    let holder = LockHolder {
        pid: std::process::id(),
        acquired_at: chrono::Utc::now().to_rfc3339(),
        label: label.to_string(),
    };
    let _ = write_holder_inner(file, &holder);
}

fn write_holder_inner(mut file: &File, holder: &LockHolder) -> std::io::Result<()> {
    let json = serde_json::to_string(holder).unwrap_or_default();
    file.seek(SeekFrom::Start(0))?;
    file.set_len(0)?;
    file.write_all(json.as_bytes())?;
    file.flush()?;
    Ok(())
}

fn clear_holder(mut file: &File) -> std::io::Result<()> {
    file.set_len(0)?;
    file.flush()?;
    Ok(())
}

/// Read the holder metadata a blocked waiter can report. Lenient: any read or
/// parse failure (missing file, torn write, legacy empty lock file) yields
/// `None` rather than an error, because this is diagnostic-only.
fn read_holder(path: &Path) -> Option<LockHolder> {
    let mut raw = String::new();
    File::open(path).ok()?.read_to_string(&mut raw).ok()?;
    serde_json::from_str(&raw).ok()
}

fn holder_suffix(path: &Path) -> String {
    match read_holder(path) {
        Some(holder) => format!(
            " (held by pid {} since {}, op: {})",
            holder.pid, holder.acquired_at, holder.label
        ),
        None => String::new(),
    }
}

fn holder_description(path: &Path) -> String {
    match read_holder(path) {
        Some(holder) => format!(
            "pid {} since {} ({})",
            holder.pid, holder.acquired_at, holder.label
        ),
        None => "unknown".to_string(),
    }
}

#[cfg(test)]
mod tests;
