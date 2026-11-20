//! A host-side copy of completed GitHub Actions logs, so paging one log in
//! windows downloads it once instead of once per window.
//!
//! The cache holds the *source* bytes `gh run view --log*` printed, never a
//! rendered excerpt: every read replays them through the same collector, so the
//! excerpt, the selection and the redaction an agent receives are exactly what
//! an uncached read would have produced.
//!
//! Only complete logs are stored. The caller learns completion from GitHub's
//! status for the job or run before it stores anything (see `logs.rs`), and a
//! partial download never reaches this module: bytes arrive only after the
//! stream ended cleanly.
//!
//! Limits (see `docs/CONFIG.md`, "GitHub run-log cache"): the cache directory
//! holds at most [`DEFAULT_MAX_TOTAL_BYTES`] and an entry lives at most
//! [`DEFAULT_MAX_AGE`] from the moment it was written. Every store removes
//! entries past either bound, oldest first, and a lookup removes the entry it
//! finds expired.
//!
//! Entries live under `<Orbit data root>/cache/github-run-logs/`, in a `0700`
//! directory, as `0600` files written through a staging file and an atomic
//! rename. The logs are stored as GitHub served them, before redaction, so
//! the directory is as sensitive as the logs themselves; nothing outside the
//! owning user can read it. Any failure to read or write the cache is a miss,
//! never a read error.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use sha2::{Digest, Sha256};

/// Total bytes the cache directory may hold. A log is at most 8 MiB (the
/// source read limit), so this keeps about thirty-two of the largest.
pub(crate) const DEFAULT_MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024;

/// How long a cached log may be served after it was written. Agents page one
/// log within minutes; a day covers a long investigation and bounds disk use.
pub(crate) const DEFAULT_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// Name of the directory under the Orbit data root.
const CACHE_DIRECTORY: [&str; 2] = ["cache", "github-run-logs"];

const ENTRY_EXTENSION: &str = "log";

/// What one cached log is the log *of*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LogCacheKey(String);

impl LogCacheKey {
    /// One completed job's log. GitHub gives every attempt's jobs new job ids,
    /// so a completed job id names one immutable log whatever the run does
    /// afterwards.
    pub(crate) fn job(repository: &str, run_id: &str, job_id: &str, scope: &str) -> Self {
        Self::digest(&["job", repository, run_id, job_id, scope])
    }

    /// One completed attempt of a run. A rerun keeps the run id, so the attempt
    /// is what keeps its log apart from the earlier one.
    pub(crate) fn run_attempt(repository: &str, run_id: &str, attempt: u64, scope: &str) -> Self {
        Self::digest(&["run", repository, run_id, &attempt.to_string(), scope])
    }

    fn digest(parts: &[&str]) -> Self {
        let mut hasher = Sha256::new();
        for part in parts {
            hasher.update(part.as_bytes());
            hasher.update([0]);
        }
        Self(
            hasher
                .finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        )
    }
}

/// A bounded directory of completed logs.
#[derive(Debug, Clone)]
pub(crate) struct LogCache {
    directory: PathBuf,
    max_total_bytes: u64,
    max_age: Duration,
}

impl LogCache {
    pub(crate) fn new(directory: PathBuf, max_total_bytes: u64, max_age: Duration) -> Self {
        Self {
            directory,
            max_total_bytes,
            max_age,
        }
    }

    /// The default cache under the Orbit data root: `ORBIT_ROOT` when set,
    /// else the machine-global `~/.orbit`. `None` when neither resolves, which
    /// leaves reads uncached.
    pub(crate) fn in_data_root() -> Option<Self> {
        let root = match std::env::var("ORBIT_ROOT") {
            Ok(root) if !root.trim().is_empty() => PathBuf::from(root),
            _ => orbit_common::fs::path::global_orbit_dir().ok()?,
        };
        let directory = CACHE_DIRECTORY
            .iter()
            .fold(root, |path, part| path.join(part));
        Some(Self::new(
            directory,
            DEFAULT_MAX_TOTAL_BYTES,
            DEFAULT_MAX_AGE,
        ))
    }

    fn entry_path(&self, key: &LogCacheKey) -> PathBuf {
        self.directory.join(format!("{}.{ENTRY_EXTENSION}", key.0))
    }

    /// The cached source bytes for `key`, or `None` on a miss. An entry past
    /// the age bound, or one that is not a plain file within the size bound, is
    /// removed and reported as a miss.
    pub(crate) fn get(&self, key: &LogCacheKey) -> Option<Vec<u8>> {
        let path = self.entry_path(key);
        let file = orbit_common::fs::open_read_only_no_follow(&path).ok()?;
        let metadata = file.metadata().ok()?;
        if !metadata.is_file()
            || metadata.len() > self.max_total_bytes
            || self.expired(&metadata, SystemTime::now())
        {
            let _ = fs::remove_file(&path);
            return None;
        }
        let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).ok()?);
        file.take(self.max_total_bytes)
            .read_to_end(&mut bytes)
            .ok()?;
        Some(bytes)
    }

    /// Store `bytes` under `key`, then remove entries past either bound.
    /// A log larger than the whole cache is not stored. Failures are logged
    /// and otherwise ignored: the caller already holds the log it read.
    pub(crate) fn put(&self, key: &LogCacheKey, bytes: &[u8]) {
        if bytes.is_empty() || bytes.len() as u64 > self.max_total_bytes {
            return;
        }
        if let Err(error) = self.write_entry(key, bytes) {
            tracing::debug!(%error, "github run log cache write failed");
            return;
        }
        self.prune(&self.entry_path(key));
    }

    /// Stage in the cache directory (a private temporary file), flush, then
    /// rename over the entry name, so a reader sees all of an entry or none.
    fn write_entry(&self, key: &LogCacheKey, bytes: &[u8]) -> std::io::Result<()> {
        use std::io::Write;

        orbit_common::fs::io::create_private_dir_all(&self.directory)?;
        let mut staged = tempfile::Builder::new()
            .prefix(".staging-")
            .tempfile_in(&self.directory)?;
        staged.write_all(bytes)?;
        orbit_common::fs::io::sync_file(staged.as_file())?;
        staged
            .persist(self.entry_path(key))
            .map_err(|error| error.error)?;
        Ok(())
    }

    fn expired(&self, metadata: &fs::Metadata, now: SystemTime) -> bool {
        metadata
            .modified()
            .ok()
            .and_then(|written| now.duration_since(written).ok())
            .is_some_and(|age| age > self.max_age)
    }

    /// Remove everything past the age bound (staging files a crashed write left
    /// behind included), then the oldest entries until the directory fits the
    /// size bound. `keep` is the entry just written and goes last.
    fn prune(&self, keep: &Path) {
        let Ok(entries) = fs::read_dir(&self.directory) else {
            return;
        };
        let now = SystemTime::now();
        let mut live: Vec<(SystemTime, u64, PathBuf)> = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if !metadata.is_file() {
                continue;
            }
            if self.expired(&metadata, now) {
                let _ = fs::remove_file(&path);
                continue;
            }
            let written = metadata.modified().unwrap_or(now);
            live.push((written, metadata.len(), path));
        }
        let mut total: u64 = live.iter().map(|(_, size, _)| size).sum();
        live.sort_by_key(|(written, _, path)| (path == keep, *written));
        for (_, size, path) in live {
            if total <= self.max_total_bytes {
                break;
            }
            if fs::remove_file(&path).is_ok() {
                total = total.saturating_sub(size);
            }
        }
    }
}
