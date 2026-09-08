//! Durable commit/recovery protocol for a multi-file task-bundle update.
//!
//! `.pending-write.yaml` is the recovery evidence. It records jsonl lengths,
//! the pre-image envelope digest, and document bodies *before* mutation.
//!
//! 1. Write the pending record (durable).
//! 2. Apply document rewrites and jsonl appends.
//! 3. COMMIT POINT: durable `task.yaml` publish (temp `fsync`, rename, parent
//!    `fsync`). After this, the envelope is the new truth.
//! 4. Remove the pending record (best-effort). A leftover pending file after
//!    a changed envelope digest means commit succeeded.
//!
//! Failure before commit, in-process or on crash: abort restores documents,
//! truncates jsonl to the recorded lengths, and removes pending. The bundle
//! returns to its pre-call state.
//!
//! Failure during abort/recovery: pending remains. The next recover retries.
//! A settled event/envelope mismatch with no pending file is corruption.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::fs::io::{StagedTextFile, atomic_write_text, with_exclusive_file_lock};
use orbit_types::task::{
    TASK_ACCEPTANCE_FILE_NAME, TASK_COMMENTS_FILE_NAME, TASK_DESCRIPTION_FILE_NAME,
    TASK_ENVELOPE_FILE_NAME, TASK_EVENTS_FILE_NAME, TASK_EXECUTION_SUMMARY_FILE_NAME,
    TASK_PLAN_FILE_NAME, TaskEnvelopeV2,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{TaskBundleV2, read_bundle_at, read_required_text, scan_jsonl_records};
use crate::fs::yaml::{serialize_yaml_with, write_yaml_durable_with};

pub(crate) const PENDING_WRITE_FILE_NAME: &str = ".pending-write.yaml";
const PENDING_WRITE_SCHEMA_VERSION: u32 = 1;

const DOCUMENT_FILES: [&str; 4] = [
    TASK_DESCRIPTION_FILE_NAME,
    TASK_ACCEPTANCE_FILE_NAME,
    TASK_PLAN_FILE_NAME,
    TASK_EXECUTION_SUMMARY_FILE_NAME,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum BundleWriteFault {
    AfterJsonlAppend,
    AfterEnvelopeStage,
    DuringCompensation,
    DuringRecovery,
}

#[cfg(test)]
thread_local! {
    static INJECTED_FAULTS: std::cell::RefCell<std::collections::HashSet<BundleWriteFault>> =
        std::cell::RefCell::new(std::collections::HashSet::new());
}

#[cfg(test)]
pub(crate) fn inject_bundle_write_faults(faults: &[BundleWriteFault]) {
    INJECTED_FAULTS.with(|cell| {
        *cell.borrow_mut() = faults.iter().copied().collect();
    });
}

pub(crate) fn fail_if_injected(_fault: BundleWriteFault) -> Result<(), OrbitError> {
    #[cfg(test)]
    {
        let hit = INJECTED_FAULTS.with(|cell| cell.borrow_mut().remove(&_fault));
        if hit {
            return Err(OrbitError::Store(format!("injected failure at {_fault:?}")));
        }
    }
    Ok(())
}

#[cfg(test)]
fn clear_injected_faults() {
    inject_bundle_write_faults(&[]);
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct PendingWrite {
    schema_version: u32,
    events_len: u64,
    comments_len: u64,
    envelope_sha256: String,
    documents: BTreeMap<String, String>,
}

pub(crate) struct PendingWriteGuard {
    bundle_dir: PathBuf,
    pending: PendingWrite,
    committed: bool,
}

impl PendingWriteGuard {
    pub(crate) fn begin(bundle_dir: &Path) -> Result<Self, OrbitError> {
        recover_pending_write(bundle_dir)?;
        let pending = snapshot_pending(bundle_dir)?;
        write_pending(bundle_dir, &pending)?;
        Ok(Self {
            bundle_dir: bundle_dir.to_path_buf(),
            pending,
            committed: false,
        })
    }

    /// Envelope publish is the commit point. Abort will not run after this.
    pub(crate) fn commit(&mut self) {
        self.committed = true;
    }

    pub(crate) fn finish(&mut self) {
        self.commit();
        self.remove_pending();
    }

    pub(crate) fn remove_pending(&self) {
        if let Err(error) = remove_pending_file(&self.bundle_dir) {
            orbit_common::tracing::warn!(
                target: "orbit.store.task_bundle_v2",
                bundle_dir = %self.bundle_dir.display(),
                error = %error,
                "failed to remove pending-write record after commit",
            );
        }
    }
}

impl Drop for PendingWriteGuard {
    fn drop(&mut self) {
        if self.committed {
            #[cfg(test)]
            clear_injected_faults();
            return;
        }
        if fail_if_injected(BundleWriteFault::DuringCompensation).is_err() {
            orbit_common::tracing::warn!(
                target: "orbit.store.task_bundle_v2",
                bundle_dir = %self.bundle_dir.display(),
                "injected compensation failure; pending-write record retained",
            );
            return;
        }
        if let Err(error) = abort_pending(&self.bundle_dir, &self.pending) {
            orbit_common::tracing::warn!(
                target: "orbit.store.task_bundle_v2",
                bundle_dir = %self.bundle_dir.display(),
                error = %error,
                "failed to abort an uncommitted bundle write; pending-write record retained",
            );
        } else {
            #[cfg(test)]
            clear_injected_faults();
        }
    }
}

/// Recover a leftover pending write under the bundle lock, then read.
pub(crate) fn recover_pending_bundle_at(bundle_dir: &Path) -> Result<TaskBundleV2, OrbitError> {
    with_exclusive_file_lock(
        &crate::driver::file::task_bundle::bundle_lock_target(bundle_dir),
        "task artifact v2",
        || {
            recover_pending_write(bundle_dir)?;
            read_bundle_at(bundle_dir)
        },
    )
}

pub(crate) fn recover_pending_write(bundle_dir: &Path) -> Result<(), OrbitError> {
    let Some(pending) = read_pending(bundle_dir)? else {
        return Ok(());
    };
    fail_if_injected(BundleWriteFault::DuringRecovery)?;
    let current = envelope_sha256(bundle_dir)?;
    if current == pending.envelope_sha256 {
        abort_pending(bundle_dir, &pending)?;
    } else {
        remove_pending_file(bundle_dir)?;
    }
    Ok(())
}

/// In-memory abort view of an incomplete pending write so listing does not
/// fail-fast. Persistence is [`recover_pending_write`].
pub(crate) fn apply_pending_read_view(
    bundle_dir: &Path,
    bundle: &mut TaskBundleV2,
) -> Result<(), OrbitError> {
    let Some(pending) = read_pending(bundle_dir)? else {
        return Ok(());
    };
    if envelope_sha256(bundle_dir)? != pending.envelope_sha256 {
        return Ok(());
    }
    bundle.events = read_jsonl_prefix(&bundle_dir.join(TASK_EVENTS_FILE_NAME), pending.events_len)?;
    bundle.comments = read_jsonl_prefix(
        &bundle_dir.join(TASK_COMMENTS_FILE_NAME),
        pending.comments_len,
    )?;
    if let Some(value) = pending.documents.get(TASK_DESCRIPTION_FILE_NAME) {
        bundle.description = value.clone();
    }
    if let Some(value) = pending.documents.get(TASK_ACCEPTANCE_FILE_NAME) {
        bundle.acceptance = value.clone();
    }
    if let Some(value) = pending.documents.get(TASK_PLAN_FILE_NAME) {
        bundle.plan = value.clone();
    }
    if let Some(value) = pending.documents.get(TASK_EXECUTION_SUMMARY_FILE_NAME) {
        bundle.execution_summary = value.clone();
    }
    Ok(())
}

pub(crate) fn publish_envelope(path: &Path, envelope: &TaskEnvelopeV2) -> Result<(), OrbitError> {
    let yaml = serialize_yaml_with(envelope, |err| OrbitError::Store(err.to_string()))?;
    let mut staged =
        StagedTextFile::new(path, &yaml).map_err(|err| OrbitError::from_write_io(path, err))?;
    fail_if_injected(BundleWriteFault::AfterEnvelopeStage)?;
    staged
        .commit()
        .map_err(|err| OrbitError::from_write_io(path, err))
}

fn snapshot_pending(bundle_dir: &Path) -> Result<PendingWrite, OrbitError> {
    let mut documents = BTreeMap::new();
    for name in DOCUMENT_FILES {
        documents.insert(
            name.to_string(),
            read_required_text(&bundle_dir.join(name))?,
        );
    }
    Ok(PendingWrite {
        schema_version: PENDING_WRITE_SCHEMA_VERSION,
        events_len: existing_file_len(&bundle_dir.join(TASK_EVENTS_FILE_NAME))?,
        comments_len: existing_file_len(&bundle_dir.join(TASK_COMMENTS_FILE_NAME))?,
        envelope_sha256: envelope_sha256(bundle_dir)?,
        documents,
    })
}

fn write_pending(bundle_dir: &Path, pending: &PendingWrite) -> Result<(), OrbitError> {
    write_yaml_durable_with(&pending_path(bundle_dir), pending, |err| {
        OrbitError::Store(err.to_string())
    })
}

fn read_pending(bundle_dir: &Path) -> Result<Option<PendingWrite>, OrbitError> {
    let path = pending_path(bundle_dir);
    match fs::read_to_string(&path) {
        Ok(raw) => {
            let pending: PendingWrite = serde_yaml::from_str(&raw).map_err(|err| {
                OrbitError::Store(format!(
                    "invalid pending-write record at {}: {err}",
                    path.display()
                ))
            })?;
            if pending.schema_version != PENDING_WRITE_SCHEMA_VERSION {
                return Err(OrbitError::Store(format!(
                    "unsupported pending-write schema {} at {}",
                    pending.schema_version,
                    path.display()
                )));
            }
            Ok(Some(pending))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(OrbitError::from_write_io(&path, err)),
    }
}

fn abort_pending(bundle_dir: &Path, pending: &PendingWrite) -> Result<(), OrbitError> {
    truncate_jsonl_file(&bundle_dir.join(TASK_EVENTS_FILE_NAME), pending.events_len)?;
    truncate_jsonl_file(
        &bundle_dir.join(TASK_COMMENTS_FILE_NAME),
        pending.comments_len,
    )?;
    for (name, content) in &pending.documents {
        let path = bundle_dir.join(name);
        atomic_write_text(&path, content).map_err(|err| OrbitError::from_write_io(&path, err))?;
    }
    remove_pending_file(bundle_dir)
}

fn remove_pending_file(bundle_dir: &Path) -> Result<(), OrbitError> {
    let path = pending_path(bundle_dir);
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(OrbitError::from_write_io(&path, err)),
    }
}

fn pending_path(bundle_dir: &Path) -> PathBuf {
    bundle_dir.join(PENDING_WRITE_FILE_NAME)
}

fn envelope_sha256(bundle_dir: &Path) -> Result<String, OrbitError> {
    let path = bundle_dir.join(TASK_ENVELOPE_FILE_NAME);
    let bytes = fs::read(&path).map_err(|err| OrbitError::from_write_io(&path, err))?;
    Ok(format!("{:x}", Sha256::digest(&bytes)))
}

fn existing_file_len(path: &Path) -> Result<u64, OrbitError> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(metadata.len()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(err) => Err(OrbitError::from_write_io(path, err)),
    }
}

fn truncate_jsonl_file(path: &Path, len: u64) -> Result<(), OrbitError> {
    match OpenOptions::new().write(true).open(path) {
        Ok(file) => {
            file.set_len(len)
                .map_err(|err| OrbitError::from_write_io(path, err))?;
            file.sync_all()
                .map_err(|err| OrbitError::from_write_io(path, err))?;
            Ok(())
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound && len == 0 => Ok(()),
        Err(err) => Err(OrbitError::from_write_io(path, err)),
    }
}

fn read_jsonl_prefix<T: serde::de::DeserializeOwned>(
    path: &Path,
    len: u64,
) -> Result<Vec<T>, OrbitError> {
    let raw = read_required_text(path)?;
    let end = (len as usize).min(raw.len());
    scan_jsonl_records(path, &raw[..end])
}
