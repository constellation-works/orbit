//! A bounded host cache of completed jobs' failed-step log reads.
//!
//! Landing passes, rerun planning and PR-completion watchers re-read the
//! same failed job's log on every poll. A completed job's log for one run
//! attempt never changes, so its bounded read answer is kept under the
//! host's data root and reused, keyed by repository, run, job and run
//! attempt. A rerun is a new attempt (and new job ids), so it is a new key;
//! nothing else invalidates an entry.
//!
//! Only a readable answer from a job the provider reported completed is
//! kept: a throttled, failed or incomplete read is never cached, so it is
//! read again. The cache holds at most [`MAX_ENTRIES`] answers of at most
//! [`MAX_ENTRY_BYTES`] each, and an entry older than [`MAX_AGE`] is ignored
//! and pruned. It is best effort: an unreadable or unwritable cache reads
//! the provider exactly as before.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::super::super::operations::{CiCommitJobEvidence, CiJobStatus};

/// Answers kept at once; the oldest is pruned first.
const MAX_ENTRIES: usize = 128;
/// The largest serialized answer kept. The read itself is bounded far below
/// this, so only a malformed answer is ever refused.
const MAX_ENTRY_BYTES: usize = 256 * 1024;
/// How long one answer is reused before it ages out.
const MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const SCHEMA_VERSION: u64 = 1;

/// The identity of one completed job's log for one run attempt.
pub(super) struct JobLogKey {
    repository: String,
    run_id: u64,
    job_id: u64,
    run_attempt: u64,
}

impl JobLogKey {
    /// The key for `job`'s log, or `None` when the job is not reported
    /// completed or its repository or run attempt is unknown.
    pub(super) fn for_completed_job(
        job: &CiCommitJobEvidence,
        run_id: u64,
        job_id: u64,
    ) -> Option<Self> {
        if job.status.value() != Some(&CiJobStatus::Completed) {
            return None;
        }
        let repository = job
            .repository
            .text()
            .map(str::trim)
            .filter(|repository| !repository.is_empty())?;
        Some(Self {
            repository: repository.to_owned(),
            run_id,
            job_id,
            run_attempt: job.run_attempt.number()?,
        })
    }

    fn file(&self, data_root: &Path) -> Option<PathBuf> {
        let digest = Sha256::digest(
            format!(
                "{}\n{}\n{}\n{}",
                self.repository, self.run_id, self.job_id, self.run_attempt
            )
            .as_bytes(),
        );
        let name: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        cache_dir(data_root).map(|dir| dir.join(format!("{name}.json")))
    }

    fn matches(&self, entry: &Value) -> bool {
        entry["schema_version"] == SCHEMA_VERSION
            && entry["repository"] == self.repository.as_str()
            && entry["run_id"] == self.run_id
            && entry["job_id"] == self.job_id
            && entry["run_attempt"] == self.run_attempt
    }
}

/// `<data root>/state/ci_job_log_cache`, or `None` for a host with no data
/// root, which keeps no cache.
fn cache_dir(data_root: &Path) -> Option<PathBuf> {
    (!data_root.as_os_str().is_empty()).then(|| data_root.join("state").join("ci_job_log_cache"))
}

/// The cached answer for `key`, unless it is missing, aged out or does not
/// carry exactly this key.
pub(super) fn load(data_root: &Path, key: &JobLogKey) -> Option<Value> {
    let path = key.file(data_root)?;
    let metadata = std::fs::metadata(&path).ok()?;
    if metadata.len() > MAX_ENTRY_BYTES as u64 || aged_out(&metadata) {
        return None;
    }
    let entry: Value = serde_json::from_slice(&std::fs::read(&path).ok()?).ok()?;
    if !key.matches(&entry) {
        return None;
    }
    entry
        .get("answer")
        .filter(|answer| answer.is_object())
        .cloned()
}

/// Keep `answer` for `key`, pruning aged-out entries and the oldest beyond
/// the entry cap first. A failure is logged and otherwise ignored.
pub(super) fn store(data_root: &Path, key: &JobLogKey, answer: &Value) {
    let Some(path) = key.file(data_root) else {
        return;
    };
    let entry = json!({
        "schema_version": SCHEMA_VERSION,
        "repository": key.repository,
        "run_id": key.run_id,
        "job_id": key.job_id,
        "run_attempt": key.run_attempt,
        "answer": answer,
    });
    let Ok(text) = serde_json::to_string(&entry) else {
        return;
    };
    if text.len() > MAX_ENTRY_BYTES {
        return;
    }
    let written = path.parent().map_or(Ok(()), |dir| {
        orbit_common::fs::io::create_private_dir_all(dir)?;
        prune(dir, &path);
        orbit_common::fs::io::atomic_write_text(&path, &text)
    });
    if let Err(error) = written {
        tracing::warn!(path = %path.display(), %error, "could not cache a completed job log");
    }
}

fn aged_out(metadata: &std::fs::Metadata) -> bool {
    metadata
        .modified()
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age > MAX_AGE)
}

/// Remove aged-out entries, then the oldest until one more fits the cap.
fn prune(dir: &Path, incoming: &Path) {
    let Ok(listing) = std::fs::read_dir(dir) else {
        return;
    };
    let mut kept = Vec::new();
    for entry in listing.flatten() {
        let path = entry.path();
        if path == incoming || path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if aged_out(&metadata) {
            let _ = std::fs::remove_file(&path);
            continue;
        }
        kept.push((metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH), path));
    }
    if kept.len() < MAX_ENTRIES {
        return;
    }
    kept.sort();
    let excess = kept.len() + 1 - MAX_ENTRIES;
    for (_, path) in kept.into_iter().take(excess) {
        let _ = std::fs::remove_file(path);
    }
}
