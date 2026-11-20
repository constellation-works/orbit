//! A host-side copy of completed GitHub Actions logs, so paging one log in
//! windows downloads it once instead of once per window.
//!
//! The cache holds a *masked* copy of the bytes `gh run view --log*` printed,
//! never the source itself and never a rendered excerpt: before any byte
//! reaches the disk, staging files included, every span the response redactor
//! would rewrite is overwritten with `*` bytes of the same length
//! ([`RedactedLog`] is the only thing [`LogCache::put`] accepts). Lengths and
//! line breaks are kept, so byte and line coordinates, step sections and
//! failure regions are those of the source. Every read of a cacheable log, the
//! read that fills the cache included, replays the masked copy through the
//! same collector, so a cold and a warm read return the same result.
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
//! A removal acts on the entry it classified, not on whatever now holds the
//! name: a concurrent store may have replaced it with a fresh log. Publishing
//! and removing therefore take one advisory lock (a sibling `<directory>.lock`
//! file, so it is never an entry), and a removal re-checks, under that lock,
//! that the name still holds the file it classified.
//!
//! Entries live under `<Orbit data root>/state/github-run-logs/`
//! ([`GITHUB_RUN_LOG_CACHE_DIR`]), in a `0700` directory, as `0600` files
//! written through a staging file and an atomic rename. The directory is
//! broker-only state: every agent sandbox masks it, so a confined worker reads
//! a log only through the bounded, redacted tool response. Earlier releases
//! kept unredacted entries in `cache/github-run-logs/`; that directory is never
//! read and is removed whenever the cache is opened or an agent sandbox is
//! prepared ([`remove_legacy_github_run_log_cache`]). Any failure to read or
//! write the cache is a miss, never a read error.

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use orbit_common::security::redaction::redact_all;
use sha2::{Digest, Sha256};

/// Total bytes the cache directory may hold. A log is at most 8 MiB (the
/// source read limit), so this keeps about thirty-two of the largest.
pub(crate) const DEFAULT_MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024;

/// How long a cached log may be served after it was written. Agents page one
/// log within minutes; a day covers a long investigation and bounds disk use.
pub(crate) const DEFAULT_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// The run-log cache directory, relative to the Orbit data root. Broker-only:
/// every agent sandbox masks it, so a confined worker cannot read cached logs
/// except through the tool's response.
pub const GITHUB_RUN_LOG_CACHE_DIR: &str = "state/github-run-logs";

/// Where earlier releases kept unredacted entries, relative to the Orbit data
/// root. Never read; removed by [`remove_legacy_github_run_log_cache`].
const LEGACY_RAW_CACHE_DIR: &str = "cache/github-run-logs";

const ENTRY_EXTENSION: &str = "log";

/// What one cached log is the log *of*.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LogCacheKey(String);

impl LogCacheKey {
    /// One completed job's log. GitHub gives every attempt's jobs new job ids,
    /// so a completed job id names one immutable log whatever the run does
    /// afterwards. `source` is the host and repository GitHub resolved for the
    /// job (see `logs.rs`), never the request's `owner/name` or `{owner}/{repo}`
    /// text: the same placeholder names a different repository in each checkout.
    pub(crate) fn job(source: &str, run_id: &str, job_id: &str, scope: &str) -> Self {
        Self::digest(&["job", source, run_id, job_id, scope])
    }

    /// One completed job's whole log as the job log API serves it, which is
    /// not the bytes `gh run view --job` prints for the same job (see
    /// [`Self::job`]), so it has its own key. It does not depend on the request's
    /// scope: scope only picks which jobs may be read, not what a job's log is.
    /// `source` is as for [`Self::job`].
    pub(crate) fn job_api(source: &str, run_id: &str, job_id: &str) -> Self {
        Self::digest(&["job_api", source, run_id, job_id])
    }

    /// One completed attempt of a run. A rerun keeps the run id, so the attempt
    /// is what keeps its log apart from the earlier one.
    /// `source` is as for [`Self::job`].
    pub(crate) fn run_attempt(source: &str, run_id: &str, attempt: u64, scope: &str) -> Self {
        Self::digest(&["run", source, run_id, &attempt.to_string(), scope])
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

/// A completed log with every span the response redactor rewrites masked: the
/// only form the cache persists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RedactedLog(Vec<u8>);

impl RedactedLog {
    /// Mask the source bytes `gh` printed, byte for byte. In each line the
    /// redactor changes, the span between the first and last changed byte
    /// becomes `*` (a line that is not UTF-8 is masked whole), so what is left
    /// is text the redacted response shows anyway.
    ///
    /// `None` when masking line by line would leave something the redactor
    /// finds only across lines: such a log is not cached.
    pub(crate) fn from_source(source: &[u8]) -> Option<Self> {
        let text = String::from_utf8_lossy(source);
        let redacted = redact_all(&text);
        if redacted == text {
            return Some(Self(source.to_vec()));
        }
        let mut masked = Vec::with_capacity(source.len());
        let mut redacted_lines = String::with_capacity(redacted.len());
        for line in source.split_inclusive(|byte| *byte == b'\n') {
            let line_text = String::from_utf8_lossy(line);
            let line_redacted = redact_all(&line_text);
            if line_redacted == line_text {
                masked.extend_from_slice(line);
            } else {
                masked.extend(mask_changed_span(line, &line_redacted));
            }
            redacted_lines.push_str(&line_redacted);
        }
        (redacted_lines == redacted).then_some(Self(masked))
    }

    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

/// `line` with the bytes between its common prefix and suffix with
/// `redacted` overwritten by `*`, cut at character boundaries. A line that is
/// not UTF-8 is masked whole, its line break kept.
fn mask_changed_span(line: &[u8], redacted: &str) -> Vec<u8> {
    let Ok(text) = std::str::from_utf8(line) else {
        return line
            .iter()
            .map(|byte| if *byte == b'\n' { b'\n' } else { b'*' })
            .collect();
    };
    let (source, redacted) = (text.as_bytes(), redacted.as_bytes());
    let mut start = source
        .iter()
        .zip(redacted)
        .take_while(|(left, right)| left == right)
        .count();
    let suffix = source[start..]
        .iter()
        .rev()
        .zip(redacted[start..].iter().rev())
        .take_while(|(left, right)| left == right)
        .count();
    let mut end = source.len() - suffix;
    while !text.is_char_boundary(start) {
        start -= 1;
    }
    while !text.is_char_boundary(end) {
        end += 1;
    }
    let mut masked = line.to_vec();
    masked[start..end].fill(b'*');
    masked
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
        Some(Self::in_root(&root))
    }

    /// The default cache under `root`, after removing the unredacted cache an
    /// earlier release left there.
    pub(crate) fn in_root(root: &Path) -> Self {
        remove_legacy_github_run_log_cache(root);
        Self::new(
            root.join(GITHUB_RUN_LOG_CACHE_DIR),
            DEFAULT_MAX_TOTAL_BYTES,
            DEFAULT_MAX_AGE,
        )
    }

    fn entry_path(&self, key: &LogCacheKey) -> PathBuf {
        self.directory.join(format!("{}.{ENTRY_EXTENSION}", key.0))
    }

    /// Take the exclusive publish/remove lock. `None` when it cannot be taken;
    /// the caller then leaves the entry alone (a removal is only housekeeping).
    /// The lock is released when the returned file is dropped.
    fn lock_mutations(&self) -> Option<File> {
        let mut name = self.directory.as_os_str().to_owned();
        name.push(".lock");
        let lock_path = PathBuf::from(name);
        if let Some(parent) = lock_path.parent() {
            orbit_common::fs::io::create_private_dir_all(parent).ok()?;
        }
        let lock = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)
            .ok()?;
        lock.lock().ok()?;
        Some(lock)
    }

    /// Remove the file at `path` only while it is still the file `classified`
    /// describes. A path that now holds a different file (a fresh publication
    /// replaced the one that was classified) is left in place. Returns whether
    /// the path was removed.
    pub(crate) fn remove_if_unchanged(&self, path: &Path, classified: &fs::Metadata) -> bool {
        let Some(_lock) = self.lock_mutations() else {
            return false;
        };
        let Ok(current) = fs::symlink_metadata(path) else {
            return false;
        };
        if !same_file(&current, classified) {
            return false;
        }
        fs::remove_file(path).is_ok()
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
            self.remove_if_unchanged(&path, &metadata);
            return None;
        }
        let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).ok()?);
        file.take(self.max_total_bytes)
            .read_to_end(&mut bytes)
            .ok()?;
        Some(bytes)
    }

    /// Store `log` under `key`, then remove entries past either bound.
    /// A log larger than the whole cache is not stored. Failures are logged
    /// and otherwise ignored: the caller already holds the log it read.
    pub(crate) fn put(&self, key: &LogCacheKey, log: &RedactedLog) {
        let bytes = log.as_bytes();
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
        let _lock = self
            .lock_mutations()
            .ok_or_else(|| std::io::Error::other("github run log cache lock unavailable"))?;
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
        let mut live: Vec<(SystemTime, PathBuf, fs::Metadata)> = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if !metadata.is_file() {
                continue;
            }
            if self.expired(&metadata, now) {
                self.remove_if_unchanged(&path, &metadata);
                continue;
            }
            let written = metadata.modified().unwrap_or(now);
            live.push((written, path, metadata));
        }
        let mut total: u64 = live.iter().map(|(_, _, metadata)| metadata.len()).sum();
        live.sort_by_key(|(written, path, _)| (path == keep, *written));
        for (_, path, metadata) in live {
            if total <= self.max_total_bytes {
                break;
            }
            if self.remove_if_unchanged(&path, &metadata) {
                total = total.saturating_sub(metadata.len());
            }
        }
    }
}

/// Remove the unredacted run-log cache an earlier release kept under `root`
/// (`cache/github-run-logs/`: its entries, staging files and the lock beside
/// it). Only regular files directly in a plain directory are removed, never
/// through a link at that name, then the directory if it is left empty.
/// Best effort: a failure is logged and the next open retries.
pub fn remove_legacy_github_run_log_cache(root: &Path) {
    let directory = root.join(LEGACY_RAW_CACHE_DIR);
    let mut lock = directory.as_os_str().to_owned();
    lock.push(".lock");
    let is_plain_directory =
        fs::symlink_metadata(&directory).is_ok_and(|metadata| metadata.is_dir());
    let entries = is_plain_directory
        .then(|| fs::read_dir(&directory).ok())
        .flatten()
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path());
    for path in entries.chain(std::iter::once(PathBuf::from(lock))) {
        if !fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.is_file()) {
            continue;
        }
        if let Err(error) = fs::remove_file(&path) {
            tracing::debug!(%error, "legacy github run log cache file not removed");
        }
    }
    if is_plain_directory {
        let _ = fs::remove_dir(&directory);
    }
}

/// Whether two metadata snapshots describe the same file, not merely the same
/// name: a replaced entry differs in inode (Unix), write time or length.
fn same_file(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if left.dev() != right.dev() || left.ino() != right.ino() {
            return false;
        }
    }
    left.len() == right.len() && left.modified().ok() == right.modified().ok()
}
