//! Rotation + retention for the global JSONL tracing feed
//! (`~/.orbit/state/logs/orbit.jsonl`). [ORB-00415]
//!
//! On an always-on host the JSONL feed would otherwise grow unbounded until the
//! disk fills — which then cascades into SQLite write failures across every
//! store. This module bounds it with an opportunistic, rename-based roll plus a
//! retention sweep. Full `rotate_and_prune` (directory walk) runs from
//! long-lived entry points (`orbit mcp serve`, `orbit sweep`, `orbit web
//! serve`). Short-lived processes only stat the active file and walk archives
//! when that one size check exceeds the per-file budget.
//!
//! The active file stays at the fixed path `orbit.jsonl` — readers
//! (`orbit log tail`, the dashboard) open that exact path, so we do NOT adopt
//! `tracing-appender`'s dated active filenames. Instead, when the active file
//! exceeds the per-file budget it is renamed to a dated archive
//! (`orbit.jsonl.<UTC-timestamp>`) and the subscriber reopens a fresh active
//! file. Retention then prunes archives older than the age budget or beyond the
//! total-size budget (delete-only; no compression, to avoid pulling a
//! compression codec into `orbit-common`).
//!
//! Concurrency: rolls are rare (size-triggered) and rename is atomic. A
//! long-running process that keeps its fd open after another process rolls the
//! file continues writing valid JSONL into the renamed inode (Unix) — no
//! corruption, though those lines land in the archive rather than the new
//! active file. That trade-off is acceptable given the criterion is
//! corruption-freedom. The subscriber reopens periodically (every MiB), limiting
//! writes to a renamed inode and checking the size budget while it runs.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use chrono::Utc;

/// Days after which operational and agent log archives are deleted.
///
/// The former `runtime.log_retention_days` default, fixed when the key was removed [ORB-22808].
pub const LOG_RETENTION_DAYS: u64 = 7;
/// Mebibyte budget for operational log archives before the oldest are pruned.
///
/// The former `runtime.log_max_total_mb` default, fixed when the key was removed [ORB-22808].
pub const LOG_MAX_TOTAL_MB: u64 = 500;
/// Mebibytes the active operational log may reach before it rolls.
///
/// The former `runtime.log_max_file_mb` default, fixed when the key was removed [ORB-22808].
pub const LOG_MAX_FILE_MB: u64 = 100;
const BYTES_PER_MB: u64 = 1024 * 1024;
const SECONDS_PER_DAY: u64 = 86_400;

/// Rotation/retention budgets for the global JSONL feed. The operational
/// feed uses the fixed [`LogRotationConfig::default`]; the agent feed derives
/// its own byte budgets from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogRotationConfig {
    /// Delete archives whose mtime is older than this many days.
    pub retention_days: u64,
    /// Total byte budget across archive files; oldest are deleted first when
    /// exceeded.
    pub max_total_bytes: u64,
    /// Roll the active file once it grows beyond this many bytes.
    pub max_file_bytes: u64,
}

impl Default for LogRotationConfig {
    fn default() -> Self {
        Self {
            retention_days: LOG_RETENTION_DAYS,
            max_total_bytes: LOG_MAX_TOTAL_MB * BYTES_PER_MB,
            max_file_bytes: LOG_MAX_FILE_MB * BYTES_PER_MB,
        }
    }
}

impl LogRotationConfig {
    /// Independent agent-output budget: 200 MiB of archives, 50 MiB per file.
    /// The operational age limit applies to both feeds, but their byte budgets
    /// are independent so agent transcripts cannot evict operational evidence.
    pub fn agent_output(self) -> Self {
        Self {
            retention_days: self.retention_days,
            max_total_bytes: 200 * BYTES_PER_MB,
            max_file_bytes: 50 * BYTES_PER_MB,
        }
    }
}

/// Opportunistically roll the active log if oversized, then prune archives by
/// age and total-size budget. Best-effort: logs a warning on failure but never
/// panics or fails the caller. Intended for long-lived processes and for the
/// oversized-file path that `rotate_if_active_exceeds_budget` takes after a
/// single `metadata()` check.
pub fn rotate_and_prune(active_path: &Path, config: &LogRotationConfig) {
    if let Err(error) = maybe_roll(active_path, config) {
        tracing::warn!(
            target: "orbit.logging.rotation",
            path = %active_path.display(),
            error = %error,
            "failed to roll oversized JSONL log",
        );
    }
    if let Err(error) = prune_archives(active_path, config) {
        tracing::warn!(
            target: "orbit.logging.rotation",
            error = %error,
            "failed to prune JSONL log archives",
        );
    }
}

/// Stat the active file once and run [`rotate_and_prune`] only when it exceeds
/// the per-file budget. A missing or unreadable file is a no-op (no directory
/// walk, no config-dependent prune). Short-lived CLI processes use this on
/// first JSONL write instead of walking archives on every start.
pub(crate) fn rotate_if_active_exceeds_budget(active_path: &Path, config: &LogRotationConfig) {
    let size = match std::fs::metadata(active_path) {
        Ok(meta) => meta.len(),
        Err(_) => return,
    };
    if size > config.max_file_bytes {
        rotate_and_prune(active_path, config);
    }
}

/// Rename the active file to a dated archive when it exceeds the per-file
/// budget. Returns `Ok(())` (no-op) when the file is absent or within budget.
pub(crate) fn maybe_roll(active_path: &Path, config: &LogRotationConfig) -> std::io::Result<()> {
    let size = match std::fs::metadata(active_path) {
        Ok(meta) => meta.len(),
        Err(_) => return Ok(()), // no active file yet
    };
    if size <= config.max_file_bytes {
        return Ok(());
    }
    std::fs::rename(active_path, archive_path(active_path))
}

fn archive_path(active_path: &Path) -> PathBuf {
    let stamp = Utc::now().format("%Y%m%dT%H%M%S%3fZ");
    let name = active_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("orbit.jsonl");
    active_path.with_file_name(format!("{name}.{stamp}"))
}

/// Delete archives older than the age budget, then, if the surviving archive
/// set still exceeds the total-size budget, delete oldest-first until it fits.
pub(crate) fn prune_archives(
    active_path: &Path,
    config: &LogRotationConfig,
) -> std::io::Result<()> {
    let Some(dir) = active_path.parent() else {
        return Ok(());
    };
    let active_name = active_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("orbit.jsonl");
    // Archives are `<active_name>.<stamp>`; never touch the active file itself.
    let prefix = format!("{active_name}.");

    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(std::io::Error::new(
                error.kind(),
                format!("read log archive directory `{}`: {error}", dir.display()),
            ));
        }
    };

    let mut archives: Vec<(PathBuf, SystemTime, u64)> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!(
                    "read entry from log archive directory `{}`: {error}",
                    dir.display()
                ),
            )
        })?;
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if !name.starts_with(&prefix) {
            continue;
        }
        let meta = entry.metadata()?;
        if !meta.is_file() {
            continue;
        }
        let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        archives.push((entry.path(), mtime, meta.len()));
    }

    // Age-based pruning. Saturate the multiply so an absurd (but validated
    // nonzero) retention_days cannot overflow into a tiny cutoff.
    if let Some(cutoff) = SystemTime::now().checked_sub(Duration::from_secs(
        config.retention_days.saturating_mul(SECONDS_PER_DAY),
    )) {
        archives.retain(|(path, mtime, _size)| {
            if *mtime < cutoff {
                let _ = std::fs::remove_file(path);
                false
            } else {
                true
            }
        });
    }

    // Size-based pruning: delete oldest archives until under the total budget.
    let mut total: u64 = archives.iter().map(|(_, _, size)| *size).sum();
    if total > config.max_total_bytes {
        archives.sort_by_key(|(_, mtime, _)| *mtime); // oldest first
        for (path, _, size) in &archives {
            if total <= config.max_total_bytes {
                break;
            }
            if std::fs::remove_file(path).is_ok() {
                total = total.saturating_sub(*size);
            }
        }
    }

    Ok(())
}
