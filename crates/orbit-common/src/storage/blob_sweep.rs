//! Filesystem half of the audit blob sweep: listing blobs and pending
//! markers, and removing one blob only if nothing protects it.
//!
//! The sweep runs beside live writers without a shared lock, so a blob that a
//! row is about to name must never be removed. Three rules close that window:
//!
//! - A writer records `<audit_root>/pending/<hash>` *before* it writes or
//!   reuses the blob, and retires the marker only after the row naming the
//!   blob commits ([`super::blob_store::BlobStore::clear_published`]).
//! - Writing content that is already stored refreshes the blob's mtime.
//! - The sweep removes a blob by first moving it to `<audit_root>/sweep/`,
//!   then rechecking the marker and the moved file's mtime. A marker or a
//!   fresh mtime moves it back; otherwise the moved file is unlinked.
//!
//! A writer that marks before the sweep's recheck is seen by it. A writer
//! that marks after it finds the blob gone and writes it again. A writer that
//! marked, published and cleared between the sweep's reference scan and its
//! recheck refreshed the blob's mtime on the way, which keeps it inside the
//! grace window the caller passes. Markers older than the caller's stale
//! cutoff belong to writers that never published (a crash, a dropped event)
//! and are reclaimed with their blobs.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::fs::io::{
    FileLockGuard, atomic_write_private_bytes, create_private_dir_all,
    try_acquire_exclusive_file_lock,
};

const BLOBS_DIR: &str = "blobs";
const PENDING_DIR: &str = "pending";
const QUARANTINE_DIR: &str = "sweep";
const SWEEP_LOCK: &str = "sweep.lock";

/// The pending-publication root of an audit root, where
/// [`super::blob_store::BlobStore::with_pending_root`] records markers.
pub fn pending_root(audit_root: &Path) -> PathBuf {
    audit_root.join(PENDING_DIR)
}

/// A lowercase SHA-256 digest, the only name a stored blob or marker has.
pub fn is_blob_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// Call `f` with every blob hash token in `text`: a maximal run of hex digits
/// exactly 64 long. Writers clearing markers and the sweep marking references
/// share this tokenizer, so they never disagree on what names a blob.
pub fn for_each_blob_hash(text: &str, mut f: impl FnMut(&str)) {
    for token in text.split(|ch: char| !ch.is_ascii_hexdigit()) {
        if is_blob_hash(token) {
            f(token);
        }
    }
}

pub(super) fn mark_pending(pending_root: &Path, hash: &str) -> io::Result<()> {
    // The marker's own mtime is its age; a rewrite replaces the file, so a
    // refreshed marker is always fresh.
    atomic_write_private_bytes(&pending_root.join(hash), b"")
}

pub(super) fn clear_pending(pending_root: &Path, published: &str) {
    for_each_blob_hash(published, |hash| {
        match fs::remove_file(pending_root.join(hash)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(%hash, %error, "could not retire an audit blob pending marker");
            }
        }
    });
}

/// One stored blob or pending marker.
#[derive(Debug, Clone)]
pub struct SweepEntry {
    pub hash: String,
    pub path: PathBuf,
    pub bytes: u64,
    pub modified: SystemTime,
}

/// What [`AuditBlobRoot::remove_unless_protected`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SweepOutcome {
    Removed,
    /// A pending marker or a fresh mtime protected it; it is back in place.
    Retained,
    /// Already gone.
    Missing,
}

/// The audit root a sweep operates on: `blobs/`, `pending/`, and the sweep's
/// own quarantine directory and lock beside them.
pub struct AuditBlobRoot {
    audit_root: PathBuf,
}

/// Held for one sweep; a second sweep of the same root is refused.
pub struct SweepLock {
    _guard: FileLockGuard,
}

impl AuditBlobRoot {
    pub fn new(audit_root: impl Into<PathBuf>) -> Self {
        Self {
            audit_root: audit_root.into(),
        }
    }

    pub fn blobs_dir(&self) -> PathBuf {
        self.audit_root.join(BLOBS_DIR)
    }

    pub fn pending_dir(&self) -> PathBuf {
        pending_root(&self.audit_root)
    }

    fn quarantine_dir(&self) -> PathBuf {
        self.audit_root.join(QUARANTINE_DIR)
    }

    /// Take the sweep lock, or `None` while another sweep holds it.
    pub fn try_lock(&self) -> io::Result<Option<SweepLock>> {
        Ok(
            try_acquire_exclusive_file_lock(&self.audit_root.join(SWEEP_LOCK), "audit blob sweep")?
                .map(|guard| SweepLock { _guard: guard }),
        )
    }

    /// Every stored blob, `blobs/<hash[..2]>/<hash>`. Staging files and
    /// anything else not named by a digest are skipped.
    pub fn blobs(&self) -> io::Result<Vec<SweepEntry>> {
        let mut entries = Vec::new();
        for shard in read_dir_if_exists(&self.blobs_dir())? {
            let shard = shard?;
            if !shard.file_type()?.is_dir() {
                continue;
            }
            for entry in fs::read_dir(shard.path())? {
                if let Some(entry) = sweep_entry(entry?)? {
                    entries.push(entry);
                }
            }
        }
        Ok(entries)
    }

    /// Every pending marker.
    pub fn pending(&self) -> io::Result<Vec<SweepEntry>> {
        let mut entries = Vec::new();
        for entry in read_dir_if_exists(&self.pending_dir())? {
            if let Some(entry) = sweep_entry(entry?)? {
                entries.push(entry);
            }
        }
        Ok(entries)
    }

    /// Put back what an interrupted sweep left in quarantine: a blob whose
    /// path is empty returns there, a duplicate of a rewritten blob is
    /// dropped. Call under the sweep lock before listing.
    pub fn recover_quarantine(&self) -> io::Result<usize> {
        let mut recovered = 0;
        for entry in read_dir_if_exists(&self.quarantine_dir())? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(hash) = name.to_str().filter(|name| is_blob_hash(name)) else {
                continue;
            };
            let target = self.blob_path(hash);
            if target.exists() {
                fs::remove_file(entry.path())?;
            } else {
                create_private_dir_all(target.parent().unwrap_or(&self.audit_root))?;
                fs::rename(entry.path(), &target)?;
            }
            recovered += 1;
        }
        Ok(recovered)
    }

    /// Remove `blob` unless, after it is moved aside, a pending marker newer
    /// than `marker_stale_before` names it or its mtime is at or after
    /// `fresh_after`.
    pub fn remove_unless_protected(
        &self,
        blob: &SweepEntry,
        fresh_after: SystemTime,
        marker_stale_before: SystemTime,
    ) -> io::Result<SweepOutcome> {
        let quarantine = self.quarantine_dir();
        create_private_dir_all(&quarantine)?;
        let moved = quarantine.join(&blob.hash);
        match fs::rename(&blob.path, &moved) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(SweepOutcome::Missing);
            }
            Err(error) => return Err(error),
        }
        let marked = modified_if_exists(&self.pending_dir().join(&blob.hash))?
            .is_some_and(|modified| modified >= marker_stale_before);
        let touched = fs::metadata(&moved)?.modified()? >= fresh_after;
        if marked || touched {
            // A writer may have stored it again meanwhile; the content is the
            // same, so either copy is the blob.
            fs::rename(&moved, &blob.path)?;
            return Ok(SweepOutcome::Retained);
        }
        fs::remove_file(&moved)?;
        Ok(SweepOutcome::Removed)
    }

    /// Remove a pending marker that is still older than `stale_before`.
    pub fn remove_stale_marker(
        &self,
        marker: &SweepEntry,
        stale_before: SystemTime,
    ) -> io::Result<bool> {
        match modified_if_exists(&marker.path)? {
            Some(modified) if modified < stale_before => match fs::remove_file(&marker.path) {
                Ok(()) => Ok(true),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
                Err(error) => Err(error),
            },
            _ => Ok(false),
        }
    }

    fn blob_path(&self, hash: &str) -> PathBuf {
        self.blobs_dir().join(&hash[..2]).join(hash)
    }
}

fn read_dir_if_exists(path: &Path) -> io::Result<Vec<io::Result<fs::DirEntry>>> {
    match fs::read_dir(path) {
        Ok(entries) => Ok(entries.collect()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error),
    }
}

fn sweep_entry(entry: fs::DirEntry) -> io::Result<Option<SweepEntry>> {
    let name = entry.file_name();
    let Some(hash) = name.to_str().filter(|name| is_blob_hash(name)) else {
        return Ok(None);
    };
    let metadata = match entry.metadata() {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !metadata.is_file() {
        return Ok(None);
    }
    Ok(Some(SweepEntry {
        hash: hash.to_string(),
        path: entry.path(),
        bytes: metadata.len(),
        modified: metadata.modified()?,
    }))
}

fn modified_if_exists(path: &Path) -> io::Result<Option<SystemTime>> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(Some(metadata.modified()?)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}
