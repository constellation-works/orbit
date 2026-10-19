use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::fs::io::open_private_file;

#[cfg(doc)]
use super::FileLockOptions;

/// Infix after the lock file's name that names a shared holder's record
/// beside it: `<lock>.holder-<pid>-<seq>`. Records are files, not a
/// directory, because a lock's parent may be a store partition whose
/// subdirectories are read as its records; a dot-prefixed lock's records stay
/// dot-prefixed infrastructure beside it.
const SHARED_HOLDER_INFIX: &str = ".holder-";

/// Most shared holders one diagnostic line names before summarizing the rest.
const MAX_NAMED_SHARED_HOLDERS: usize = 8;

static SHARED_HOLDER_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Advisory metadata recorded by a lock holder: in the lock file by an
/// exclusive holder, beside it by a shared one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileLockHolderInfo {
    /// Process ID of the holder that wrote this metadata.
    pub pid: u32,
    /// RFC 3339 acquisition timestamp.
    pub acquired_at: String,
    /// Human-readable operation label.
    pub label: String,
}

/// Read advisory holder metadata. Missing, empty, torn, and legacy files are
/// intentionally reported as no metadata because the OS lock is authoritative.
pub fn read_file_lock_holder(lock_path: &Path) -> Option<FileLockHolderInfo> {
    read_file_lock_holder_with_hook(lock_path, |_| {})
}

/// Read the records shared holders keep beside `lock_path`, oldest first.
///
/// Only a lock acquired with [`FileLockOptions::record_shared_holders`] has
/// any. A record whose process is gone is a crashed holder's leftover — the
/// OS released its lock with the process — so it is skipped and removed.
/// Torn and unreadable records are skipped, like the exclusive record.
pub(super) fn read_shared_file_lock_holders(lock_path: &Path) -> Vec<FileLockHolderInfo> {
    let (Some(parent), Some(prefix)) = (
        validated_lock_holder_parent(lock_path),
        shared_holder_prefix(lock_path),
    ) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&parent) else {
        return Vec::new();
    };
    let mut holders = Vec::new();
    for entry in entries.flatten() {
        if !entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(&prefix))
        {
            continue;
        }
        let path = entry.path();
        let Some(file) = open_lock_holder_file(&path, |_| {}) else {
            continue;
        };
        let Some(holder) = read_holder_record(file) else {
            continue;
        };
        if holder_process_gone(holder.pid) {
            let _ = std::fs::remove_file(&path);
        } else {
            holders.push(holder);
        }
    }
    holders.sort_by(|left, right| left.acquired_at.cmp(&right.acquired_at));
    holders
}

/// Whether a recorded holder's process has exited. Only Unix can tell; a
/// record elsewhere is kept and its holder named.
fn holder_process_gone(pid: u32) -> bool {
    #[cfg(unix)]
    {
        !crate::process::identity::process_is_alive(pid)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

fn read_holder_record(mut file: File) -> Option<FileLockHolderInfo> {
    let mut raw = String::new();
    file.read_to_string(&mut raw).ok()?;
    serde_json::from_str(&raw).ok()
}

fn shared_holder_prefix(lock_path: &Path) -> Option<String> {
    let name = lock_path.file_name()?.to_str()?;
    Some(format!("{name}{SHARED_HOLDER_INFIX}"))
}

/// Record this shared holder beside the lock. Best effort, like the
/// exclusive record: the OS lock is authoritative, and a store that refuses
/// the record still serves the read. The record goes in the canonical lock
/// directory that readers scan, so an aliased lock path is read where it was
/// written.
pub(super) fn write_shared_holder_record(lock_path: &Path, label: &str) -> Option<PathBuf> {
    let prefix = shared_holder_prefix(lock_path)?;
    let sequence = SHARED_HOLDER_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let path = validated_lock_holder_parent(lock_path)?
        .join(format!("{prefix}{}-{sequence}", std::process::id()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    let file = open_private_file(&path, &mut options).ok()?;
    let holder = FileLockHolderInfo {
        pid: std::process::id(),
        acquired_at: chrono::Utc::now().to_rfc3339(),
        label: label.to_string(),
    };
    match write_file_lock_holder_inner(&file, &holder) {
        Ok(()) => Some(path),
        Err(_) => {
            let _ = std::fs::remove_file(&path);
            None
        }
    }
}

/// Test-only seam that runs `before_open` between path resolution and the
/// no-follow open, so a test can deterministically swap the resolved final
/// component for a symlink and prove the open still rejects it [ORB-12029].
#[cfg(test)]
pub(crate) fn read_file_lock_holder_after_resolve<F>(
    lock_path: &Path,
    before_open: F,
) -> Option<FileLockHolderInfo>
where
    F: FnOnce(&Path),
{
    read_file_lock_holder_with_hook(lock_path, before_open)
}

fn read_file_lock_holder_with_hook(
    lock_path: &Path,
    before_open: impl FnOnce(&Path),
) -> Option<FileLockHolderInfo> {
    read_holder_record(open_lock_holder_file(lock_path, before_open)?)
}

/// Resolve `lock_path`'s parent directory to its canonical form.
///
/// `lock_path` reaches this crate as a caller-selected value (a task lock, a
/// store lock) with no upstream containment check. Canonicalizing only the
/// parent — not the final component — keeps the reads and record writes
/// confined to the resolved parent directory while still accepting a trusted
/// parent alias (for example a checkout projection symlink) [ORB-11953].
/// Deciding whether the final component itself is safe to read is
/// [`open_lock_holder_file`]'s job, not this function's: resolving that here
/// and handing back a path left a window between this check and the caller's
/// open where the final component could be swapped for a symlink [ORB-12029].
fn validated_lock_holder_parent(lock_path: &Path) -> Option<PathBuf> {
    lock_path.parent()?.canonicalize().ok()
}

/// Open `lock_path`'s final component for a read-only diagnostic read
/// without following a symlink planted there.
///
/// The pathname `symlink_metadata` check below is a fast rejection for the
/// common case (missing file, directory, or already-a-symlink) so this
/// avoids blocking on an exotic node such as a FIFO; it is *not* the security
/// boundary. That boundary is the open immediately after: on Unix,
/// `O_NOFOLLOW` makes the open itself fail if the final component is a
/// symlink and `O_NONBLOCK` prevents a swapped FIFO from hanging the caller;
/// the follow-up `metadata()` call is an `fstat` on the
/// already-open descriptor, re-checking the file that was actually opened
/// rather than a path that could have changed again. Folding the check and
/// the open into one function, with the open re-validating its own
/// descriptor, closes the window a caller-visible "validated path" handed to
/// a separate `File::open` left open [ORB-12029]. `None` covers "nothing to
/// read", matching this function's existing no-metadata-on-missing/malformed
/// -target semantics.
///
/// This secures only the final path component. [`validated_lock_holder_parent`]
/// resolving the parent supports a trusted parent alias; it does not claim to
/// stop a party who can write to that parent from renaming or replacing the
/// lock file's directory entry through some other, ancestor-level race —
/// only the leaf-symlink swap this closes.
///
/// The no-follow open is atomic against the leaf swap on Unix (`O_NOFOLLOW`)
/// and nonblocking for a swapped FIFO. On Windows,
/// `FILE_FLAG_OPEN_REPARSE_POINT` opens a reparse point itself instead of its
/// target. On any other platform the open
/// follows a symlink normally, so the pathname pre-check above is the only
/// protection and a swap landing between that check and the open is not
/// covered there.
fn open_lock_holder_file(lock_path: &Path, before_open: impl FnOnce(&Path)) -> Option<File> {
    let file_name = lock_path.file_name()?;
    let canonical_parent = validated_lock_holder_parent(lock_path)?;
    let candidate = canonical_parent.join(file_name);

    match std::fs::symlink_metadata(&candidate) {
        Ok(metadata) if metadata.is_file() => {}
        _ => return None,
    }

    before_open(&candidate);

    let file = crate::fs::open_read_only_no_follow(&candidate).ok()?;
    let metadata = file.metadata().ok()?;
    metadata.is_file().then_some(file)
}

/// Name the exclusive acquirer a reader is queued behind, and whoever that
/// acquirer is itself waiting on.
pub(super) fn describe_queued_exclusive_waiter(turnstile: &Path, lock_path: &Path) -> String {
    let waiter = read_file_lock_holder(turnstile).map_or_else(
        || "unknown".to_string(),
        |holder| {
            format!(
                "pid {} since {} ({})",
                holder.pid, holder.acquired_at, holder.label
            )
        },
    );
    format!(
        "queued exclusive waiter: {waiter}; waiting on {}",
        describe_file_lock_holders(lock_path)
    )
}

/// Name whoever holds `lock_path` for a contention diagnostic: the exclusive
/// holder's record, else every live shared holder's, else `unknown`.
pub(super) fn describe_file_lock_holders(lock_path: &Path) -> String {
    if let Some(holder) = read_file_lock_holder(lock_path) {
        return format!(
            "pid {} since {} ({})",
            holder.pid, holder.acquired_at, holder.label
        );
    }
    let shared = read_shared_file_lock_holders(lock_path);
    if shared.is_empty() {
        return "unknown".to_string();
    }
    format!("shared: {}", describe_shared_holders(&shared))
}

pub(super) fn describe_shared_holders(holders: &[FileLockHolderInfo]) -> String {
    let mut described = holders
        .iter()
        .take(MAX_NAMED_SHARED_HOLDERS)
        .map(|holder| {
            format!(
                "pid {} since {} ({})",
                holder.pid, holder.acquired_at, holder.label
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    if holders.len() > MAX_NAMED_SHARED_HOLDERS {
        described.push_str(&format!(
            "; {} more",
            holders.len() - MAX_NAMED_SHARED_HOLDERS
        ));
    }
    described
}

pub(super) fn write_file_lock_holder(file: &File, label: &str) {
    let holder = FileLockHolderInfo {
        pid: std::process::id(),
        acquired_at: chrono::Utc::now().to_rfc3339(),
        label: label.to_string(),
    };
    let _ = write_file_lock_holder_inner(file, &holder);
}

fn write_file_lock_holder_inner(mut file: &File, holder: &FileLockHolderInfo) -> io::Result<()> {
    let encoded = serde_json::to_vec(holder).map_err(io::Error::other)?;
    file.seek(SeekFrom::Start(0))?;
    file.set_len(0)?;
    file.write_all(&encoded)?;
    file.flush()
}

pub(super) fn clear_file_lock_holder(mut file: &File) -> io::Result<()> {
    file.set_len(0)?;
    file.flush()
}
