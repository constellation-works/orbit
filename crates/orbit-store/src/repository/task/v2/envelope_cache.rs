//! Freshness-stamped reuse of parsed task envelopes.
//!
//! Indexed selection validates the generated index against every registered
//! task's envelope on every listing, so the freshness scan used to re-read and
//! re-parse one `task.yaml` per registered task even when nothing had changed.
//! This cache keeps each parse and re-validates it against the envelope file's
//! stamp before every reuse, turning the steady-state scan into one metadata
//! probe per task; the rows the index then selects are served from the same
//! parses.
//!
//! # Freshness policy
//!
//! An entry is reused only when the envelope file still reports the same
//! [`EnvelopeStamp`]: filesystem identity (device and inode), byte length, and
//! modification time. The store never rewrites `task.yaml` in place — every
//! publish stages a sibling file and renames it over the target — so a write
//! from this process or any other one lands on a new inode and invalidates the
//! entry. The stamp is taken *before* the parse it labels, so a write that
//! races the read can only cost an extra parse on the next scan; superseded
//! content is never reused.
//!
//! # What a stamp does not prove
//!
//! A stamp is evidence that a file was not rewritten, not proof that its bytes
//! are identical. An in-place overwrite that preserved identity, length, and
//! timestamp would go unnoticed, and a filesystem with coarse timestamps or no
//! inode numbers narrows the evidence further. Reuse therefore never stands
//! alone: the caller still compares every reused envelope's `updated_at`
//! against the generated index, and `reindex_workspace` re-reads and
//! re-validates every bundle from disk with no reference to this cache.
//!
//! Entries are bounded by the tasks registered to the workspace, which the
//! freshness scan already materializes in full; a deleted task's entry is
//! dropped by a later scan and can never be served in the meantime, because
//! task IDs are not reused.
//!
//! # Across processes
//!
//! A fresh process starts with an empty cache, and parsing every envelope to
//! refill it made the first listing after a restart cost seconds. The scan
//! therefore also records each proof it makes — this stamp matched an index
//! row with that fingerprint — in the registry's envelope stamps, and a later
//! scan in any process accepts a task without parsing it while its file still
//! reports the recorded stamp and its index row still has the recorded
//! fingerprint. Only a stamp carrying both identity and modification time is
//! recorded, the same evidence and the same limits as reuse within a
//! process. The store's writes rename a newly created file over the old one,
//! which therefore never shares its inode, and a row rewrite changes the
//! fingerprint.

use std::collections::{BTreeSet, HashMap};
use std::fs::{self, Metadata};
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use orbit_types::task::TaskEnvelopeV2;

use crate::contracts::TaskBundleBinding;

/// Filesystem evidence that an envelope file is the one a cached parse came
/// from. See the module docs for the guarantees this does and does not carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct EnvelopeStamp {
    len: u64,
    modified: Option<SystemTime>,
    /// Device and inode on Unix; `None` where the platform reports neither.
    identity: Option<(u64, u64)>,
}

impl EnvelopeStamp {
    /// The stamp as the registry records it, or `None` when it lacks the
    /// identity or modification time a proof across processes needs.
    pub(super) fn persisted(&self) -> Option<String> {
        let modified = self.modified?.duration_since(UNIX_EPOCH).ok()?;
        let (device, inode) = self.identity?;
        Some(format!(
            "{}:{}.{:09}:{device}:{inode}",
            self.len,
            modified.as_secs(),
            modified.subsec_nanos()
        ))
    }
}

#[derive(Default)]
pub(super) struct EnvelopeCache {
    entries: Mutex<HashMap<String, (EnvelopeStamp, TaskEnvelopeV2)>>,
}

impl EnvelopeCache {
    /// Stamp `path`, or `None` when it cannot be stamped — a bundle a
    /// concurrent writer is publishing or removing, or a metadata call that
    /// failed. Both cases fall through to the strict envelope read, which
    /// decides between "skip this task" and a reported error.
    pub(super) fn stamp(&self, path: &Path) -> Option<EnvelopeStamp> {
        let metadata = fs::metadata(path).ok()?;
        Some(EnvelopeStamp {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            identity: file_identity(&metadata),
        })
    }

    /// Run `inspect` on the envelope parsed from the file this stamp
    /// describes, if it is still the file the entry was taken from. The
    /// freshness scan compares a few fields per task this way instead of
    /// cloning every envelope it does not go on to select.
    pub(super) fn inspect_fresh<T>(
        &self,
        task_id: &str,
        stamp: &EnvelopeStamp,
        inspect: impl FnOnce(&TaskEnvelopeV2) -> T,
    ) -> Option<T> {
        let entries = self.entries();
        let (cached, envelope) = entries.get(task_id)?;
        (cached == stamp).then(|| inspect(envelope))
    }

    /// The most recent parse remembered for `task_id`, without re-proving its
    /// stamp: for the ids a selection returns right after the freshness scan
    /// stamped every registered task.
    pub(super) fn cached(&self, task_id: &str) -> Option<TaskEnvelopeV2> {
        self.entries()
            .get(task_id)
            .map(|(_, envelope)| envelope.clone())
    }

    pub(super) fn remember(&self, task_id: &str, stamp: EnvelopeStamp, envelope: &TaskEnvelopeV2) {
        self.entries()
            .insert(task_id.to_string(), (stamp, envelope.clone()));
    }

    pub(super) fn forget(&self, task_id: &str) {
        self.entries().remove(task_id);
    }

    /// Drop entries for tasks the workspace no longer registers. Deletion
    /// removes the binding, so holding more entries than bindings is the
    /// signal that orphans have accumulated; entries below that threshold are
    /// left alone rather than paying a set build on every scan.
    pub(super) fn retain_registered(&self, registered: &[TaskBundleBinding]) {
        let mut entries = self.entries();
        if entries.len() <= registered.len() {
            return;
        }
        let live = registered
            .iter()
            .map(|binding| binding.task_id.as_str())
            .collect::<BTreeSet<_>>();
        entries.retain(|task_id, _| live.contains(task_id.as_str()));
    }

    /// A poisoned lock only means some thread panicked while holding it. Every
    /// entry is re-proved against the filesystem before it is reused, so the
    /// cache keeps serving rather than failing an unrelated read.
    fn entries(&self) -> MutexGuard<'_, HashMap<String, (EnvelopeStamp, TaskEnvelopeV2)>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(unix)]
fn file_identity(metadata: &Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;

    Some((metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
fn file_identity(_metadata: &Metadata) -> Option<(u64, u64)> {
    None
}
