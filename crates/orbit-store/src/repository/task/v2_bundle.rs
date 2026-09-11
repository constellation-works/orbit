//! Task bundle orchestration joins file durability and registry bindings.
//! Full-bundle reads and mutations share a persistent external lock; deletion
//! publishes a recoverable rename before cleanup.

use std::fs::{self, File};
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::fs::io::{
    atomic_write_text, sync_parent_dir, with_exclusive_file_lock, with_shared_file_lock,
};
use orbit_types::task::{
    ArtifactManifestV2, TASK_ARTIFACT_MANIFEST_FILE_NAME, TASK_ARTIFACTS_DIR_NAME,
    TASK_COMMENTS_FILE_NAME, TASK_ENVELOPE_FILE_NAME, TASK_EVENTS_FILE_NAME, TaskCommentRowV2,
    TaskEnvelopeV2, TaskEventRowV2,
};

use crate::driver::file::task_bundle::bundle_lock_target;
pub(crate) use crate::driver::file::task_bundle::{TaskBundleV2, TaskDocumentV2};
use crate::driver::file::task_bundle::{
    append_jsonl_row, cleanup_partial_bundle_best_effort, publish_envelope, read_bundle_at,
    read_bundle_lightweight_at, read_envelope_at, write_bundle_at,
};
use crate::driver::sqlite::task_registry::{TaskBundleBinding, TaskRegistryStore};
use crate::fs::yaml::write_yaml_durable_with;

fn sync_parent_path(path: &Path) -> Result<(), OrbitError> {
    let parent = path.parent().ok_or_else(|| {
        OrbitError::Store(format!("path has no parent directory: {}", path.display()))
    })?;
    let parent_dir = File::open(parent).map_err(|err| OrbitError::from_write_io(path, err))?;
    sync_parent_dir(&parent_dir).map_err(|err| OrbitError::from_write_io(path, err))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TaskBundleCreateResult {
    pub(crate) binding: TaskBundleBinding,
}

pub(crate) struct TaskBundleStoreV2 {
    // pub(crate) fields widened to allow sibling `tests/v2_bundle.rs` (and promoted
    // `tests/test_support.rs`) to access internal state for durability
    // assertions. See ORB-00247 and docs/design-patterns/test_layout.md (widen
    // deliberately rather than keep nested anti-pattern).
    #[cfg(test)]
    pub(crate) bundle_reads: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    pub(crate) envelope_reads: std::sync::atomic::AtomicUsize,
    pub(crate) registry: TaskRegistryStore,
    pub(crate) workspace_id: String,
}

impl TaskBundleStoreV2 {
    pub(crate) fn new(registry: TaskRegistryStore, workspace_id: String) -> Self {
        Self {
            #[cfg(test)]
            bundle_reads: Default::default(),
            #[cfg(test)]
            envelope_reads: Default::default(),
            registry,
            workspace_id,
        }
    }

    pub(crate) fn bundle_path(&self, task_id: &str) -> Result<PathBuf, OrbitError> {
        self.registry
            .canonical_task_bundle_path(&self.workspace_id, task_id)
    }

    /// The single file [`Self::read_envelope_if_settled`] parses, for callers
    /// that decide whether that parse is still needed.
    pub(crate) fn envelope_path(&self, task_id: &str) -> Result<PathBuf, OrbitError> {
        Ok(self.bundle_path(task_id)?.join(TASK_ENVELOPE_FILE_NAME))
    }

    /// Run `op` while holding this task's exclusive bundle lock.
    ///
    /// A lifecycle write spans more than one file — a transition appends to
    /// `events.jsonl` and republishes `task.yaml` — so it is only a consistent
    /// unit to a reader that observes the same lock. This store owns the lock
    /// target for both sides ([`bundle_lock_target`]) precisely so a reader and
    /// a writer cannot drift onto different files (ORB-11349).
    pub(crate) fn with_bundle_write_lock<T, F>(&self, task_id: &str, op: F) -> Result<T, OrbitError>
    where
        F: FnOnce() -> Result<T, OrbitError>,
    {
        with_exclusive_file_lock(
            &bundle_lock_target(&self.bundle_path(task_id)?),
            "task artifact v2",
            || {
                // A queued writer may resume after deletion. Check under the
                // stable lock before any helper can create parent directories.
                read_envelope_at(&self.bundle_path(task_id)?)?;
                op()
            },
        )
    }

    /// The caller has durably reserved this ID for one action and input digest.
    /// Re-enter after a crash under the canonical bundle lock. A readable bundle
    /// wins; unreadable bytes are retained for explicit recovery.
    pub(crate) fn create_or_recover_action_bundle(
        &self,
        proposed: &TaskBundleV2,
    ) -> Result<TaskBundleV2, OrbitError> {
        let id = &proposed.envelope.id;
        let path = self.bundle_path(id)?;
        with_exclusive_file_lock(&bundle_lock_target(&path), "task action admission", || {
            // Re-entrant read under the same lock as lifecycle writes.
            if let Ok(existing) = read_bundle_consistently(&path) {
                self.registry
                    .register_task_bundle(id, &self.workspace_id, &path)?;
                return Ok(existing);
            }
            if path.exists() {
                return Err(OrbitError::Store(
                    "action task bundle is unreadable; repair the retained bundle before replay"
                        .into(),
                ));
            }
            self.create_bundle_locked(id, &path, proposed)?;
            Ok(proposed.clone())
        })
    }

    pub(crate) fn create_bundle(
        &self,
        bundle: &TaskBundleV2,
    ) -> Result<TaskBundleCreateResult, OrbitError> {
        let task_id = bundle.envelope.id.clone();
        let bundle_dir = self.bundle_path(&task_id)?;
        with_exclusive_file_lock(
            &bundle_lock_target(&bundle_dir),
            "task bundle create",
            || self.create_bundle_locked(&task_id, &bundle_dir, bundle),
        )
    }

    fn create_bundle_locked(
        &self,
        task_id: &str,
        bundle_dir: &Path,
        bundle: &TaskBundleV2,
    ) -> Result<TaskBundleCreateResult, OrbitError> {
        if deletion_path(bundle_dir).try_exists()? {
            return Err(OrbitError::Store(
                "task deletion is pending; rerun deletion or reindex before creation".into(),
            ));
        }
        if bundle_dir.exists() {
            return Err(OrbitError::Store(format!(
                "task bundle already exists at {}",
                bundle_dir.display()
            )));
        }

        if let Err(err) = write_bundle_at(bundle_dir, bundle) {
            cleanup_partial_bundle_best_effort(bundle_dir, "bundle write", &err);
            return Err(err);
        }

        // `write_bundle_at` fsyncs each artifact into `bundle_dir`, but the
        // mkdir that created `bundle_dir` is an entry in its parent that is
        // otherwise left in the page cache. Without fsyncing the parent, a power
        // loss in the creation window can orphan the whole bundle even though
        // every file inside was durably written.
        if let Err(err) = sync_parent_path(bundle_dir) {
            cleanup_partial_bundle_best_effort(bundle_dir, "bundle dir fsync", &err);
            return Err(err);
        }

        let binding =
            match self
                .registry
                .register_task_bundle(task_id, &self.workspace_id, bundle_dir)
            {
                Ok(binding) => binding,
                Err(err) => {
                    cleanup_partial_bundle_best_effort(bundle_dir, "registry registration", &err);
                    return Err(err);
                }
            };
        Ok(TaskBundleCreateResult { binding })
    }

    /// Canonical full-bundle read: hashes every artifact payload.
    pub(crate) fn read_bundle(&self, task_id: &str) -> Result<TaskBundleV2, OrbitError> {
        #[cfg(test)]
        self.bundle_reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let bundle_dir = self.bundle_path(task_id)?;
        read_bundle_consistently(&bundle_dir)
    }

    /// Assemble a task bundle without opening artifact payload bytes.
    pub(crate) fn read_bundle_lightweight(
        &self,
        task_id: &str,
    ) -> Result<TaskBundleV2, OrbitError> {
        #[cfg(test)]
        self.bundle_reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let bundle_dir = self.bundle_path(task_id)?;
        read_bundle_lightweight_consistently(&bundle_dir)
    }

    pub(crate) fn delete_bundle(&self, task_id: &str) -> Result<bool, OrbitError> {
        orbit_types::task::validate_orb_task_id(task_id)?;
        let bundle_dir = self.bundle_path(task_id)?;
        with_exclusive_file_lock(
            &bundle_lock_target(&bundle_dir),
            "task bundle delete",
            || self.delete_bundle_locked(task_id, &bundle_dir),
        )
    }

    /// Reindex resumes only published deletions, never deletes a live bundle.
    pub(crate) fn recover_deletion(&self, task_id: &str) -> Result<bool, OrbitError> {
        let bundle_dir = self.bundle_path(task_id)?;
        with_exclusive_file_lock(
            &bundle_lock_target(&bundle_dir),
            "task deletion recovery",
            || {
                if !deletion_path(&bundle_dir).try_exists()? {
                    return Ok(false);
                }
                self.delete_bundle_locked(task_id, &bundle_dir)
            },
        )
    }

    fn delete_bundle_locked(&self, task_id: &str, bundle_dir: &Path) -> Result<bool, OrbitError> {
        let tombstone = deletion_path(bundle_dir);
        let published = tombstone.try_exists()?;
        let exists = bundle_dir.try_exists()?;
        if published && exists {
            return Err(OrbitError::Store(format!(
                "task {task_id} has both a canonical bundle and a deletion tombstone; retained both for repair"
            )));
        }
        // Rename publishes deletion before any destructive cleanup. The whole
        // bundle survives registry failure, and a cleanup failure stays outside
        // the canonical namespace. Retry always rolls a published deletion forward.
        deletion_fault(DeletionFault::Publication)?;
        if exists {
            fs::rename(bundle_dir, &tombstone)
                .map_err(|err| OrbitError::from_write_io(bundle_dir, err))?;
        }
        if exists || published {
            deletion_fault(DeletionFault::PublicationSync)?;
            sync_parent_path(&tombstone)?;
        }
        deletion_fault(DeletionFault::Registry)?;
        let unregistered = self
            .registry
            .unregister_task_bundle(task_id, &self.workspace_id)?;
        deletion_fault(DeletionFault::Cleanup)?;
        if exists || published {
            fs::remove_dir_all(&tombstone)
                .map_err(|err| OrbitError::from_write_io(&tombstone, err))?;
            sync_parent_path(&tombstone)?;
        }
        Ok(unregistered || exists || published)
    }

    /// List bundles registered to this workspace using the lightweight read.
    ///
    /// Task-field corruption is still fail-fast — one damaged envelope, body,
    /// event log, or event/envelope status mismatch fails the list so store
    /// damage is never silently hidden. Artifact payload existence, size, and
    /// sha256 are deferred; [`Self::read_bundle`] and explicit reindex, import,
    /// publication restore, and artifact retrieval still verify those bytes.
    /// An incomplete multi-file write is recognized by `.pending-write.yaml`
    /// and recovered rather than reported as damage. What is *not* fail-fast
    /// is a bundle caught mid-publication or mid-removal by a concurrent
    /// writer (ORB-10988 / F2026-07-119): the binding list is a snapshot, so a
    /// create or delete of one task would otherwise fail every read of every
    /// other task. Those bundles are skipped, exactly as they would be had
    /// the snapshot been taken a moment earlier or later.
    pub(crate) fn list_bundles(&self) -> Result<Vec<TaskBundleV2>, OrbitError> {
        let bindings = self.registry.tasks_for_workspace(&self.workspace_id)?;
        let mut bundles = Vec::with_capacity(bindings.len());
        for binding in &bindings {
            #[cfg(test)]
            self.bundle_reads
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if let Some(bundle) = read_bundle_tolerating_in_flight(&binding.canonical_path)? {
                bundles.push(bundle);
            }
        }
        Ok(bundles)
    }

    /// Read one registered bundle on the lightweight listing path, skipping it
    /// when a concurrent writer has it in flight. See [`Self::list_bundles`]
    /// for the tolerance rule and the checks this read defers.
    pub(crate) fn read_bundle_if_settled(
        &self,
        task_id: &str,
    ) -> Result<Option<TaskBundleV2>, OrbitError> {
        #[cfg(test)]
        self.bundle_reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        read_bundle_tolerating_in_flight(&self.bundle_path(task_id)?)
    }

    /// Read one registered bundle's envelope, skipping it when a concurrent
    /// writer has it in flight. See [`Self::list_bundles`] for the rule.
    pub(crate) fn read_envelope_if_settled(
        &self,
        task_id: &str,
    ) -> Result<Option<TaskEnvelopeV2>, OrbitError> {
        #[cfg(test)]
        self.envelope_reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let bundle_dir = self.bundle_path(task_id)?;
        match read_envelope_at(&bundle_dir) {
            Ok(envelope) => Ok(Some(envelope)),
            Err(err) => skip_if_in_flight(&bundle_dir, err),
        }
    }

    pub(crate) fn rewrite_document(
        &self,
        task_id: &str,
        document: TaskDocumentV2,
        content: &str,
    ) -> Result<(), OrbitError> {
        let path = self.bundle_path(task_id)?.join(document.file_name());
        atomic_write_text(&path, content).map_err(|err| OrbitError::from_write_io(&path, err))
    }

    pub(crate) fn rewrite_envelope(
        &self,
        task_id: &str,
        envelope: &TaskEnvelopeV2,
    ) -> Result<(), OrbitError> {
        if envelope.id != task_id {
            return Err(OrbitError::InvalidInput(format!(
                "task envelope id '{}' does not match target task id '{task_id}'",
                envelope.id
            )));
        }
        envelope.validate()?;
        publish_envelope(&self.envelope_path(task_id)?, envelope)
    }

    pub(crate) fn rewrite_artifact_manifest(
        &self,
        task_id: &str,
        manifest: &ArtifactManifestV2,
    ) -> Result<(), OrbitError> {
        manifest.validate()?;
        write_yaml_durable_with(
            &self
                .bundle_path(task_id)?
                .join(TASK_ARTIFACTS_DIR_NAME)
                .join(TASK_ARTIFACT_MANIFEST_FILE_NAME),
            manifest,
            |err| OrbitError::Store(err.to_string()),
        )
    }

    pub(crate) fn append_event(
        &self,
        task_id: &str,
        event: &TaskEventRowV2,
    ) -> Result<(), OrbitError> {
        event.validate()?;
        append_jsonl_row(
            &self.bundle_path(task_id)?.join(TASK_EVENTS_FILE_NAME),
            event,
        )
    }

    pub(crate) fn append_comment(
        &self,
        task_id: &str,
        comment: &TaskCommentRowV2,
    ) -> Result<(), OrbitError> {
        comment.validate()?;
        append_jsonl_row(
            &self.bundle_path(task_id)?.join(TASK_COMMENTS_FILE_NAME),
            comment,
        )
    }
}

/// A published deletion is retained here until registry/projection removal and
/// cleanup finish. Canonical and tombstone coexistence is ambiguous and retained.
pub(crate) fn deletion_path(bundle_dir: &Path) -> PathBuf {
    bundle_dir.with_extension("deleted")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DeletionFault {
    Publication,
    PublicationSync,
    Registry,
    Cleanup,
}

#[cfg(test)]
thread_local! {
    static DELETION_FAULT: std::cell::Cell<Option<DeletionFault>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(crate) fn inject_deletion_fault(fault: DeletionFault) {
    DELETION_FAULT.set(Some(fault));
}

fn deletion_fault(_fault: DeletionFault) -> Result<(), OrbitError> {
    #[cfg(test)]
    if DELETION_FAULT.get() == Some(_fault) {
        DELETION_FAULT.set(None);
        return Err(OrbitError::Store(format!(
            "injected deletion failure at {_fault:?}"
        )));
    }
    Ok(())
}

/// Assemble a whole bundle under this task's shared read lock.
///
/// A bundle spans several files, so reading it is only atomic with respect to
/// a lifecycle write that publishes across those same files if the reader
/// observes the writer's lock (ORB-11349). Without it a reader could pair an
/// appended transition event with the envelope the writer had not yet
/// republished, and report that mismatch as bundle corruption.
///
/// Envelope-only reads stay lock-free on purpose: `task.yaml` is renamed into
/// place atomically, so one file is always self-consistent, and the index
/// validation that reads it on every listing pays nothing here.
fn read_bundle_consistently(bundle_dir: &Path) -> Result<TaskBundleV2, OrbitError> {
    // A missing bundle cannot be in a lifecycle transition. Avoid asking the
    // shared-lock helper to create a stable lock target for a filtered miss.
    if !bundle_dir.try_exists()? {
        return read_bundle_at(bundle_dir);
    }

    with_shared_file_lock(&bundle_lock_target(bundle_dir), "task artifact v2", || {
        read_bundle_at(bundle_dir)
    })
}

/// Same lock as [`read_bundle_consistently`], without hashing artifact blobs.
fn read_bundle_lightweight_consistently(bundle_dir: &Path) -> Result<TaskBundleV2, OrbitError> {
    if !bundle_dir.try_exists()? {
        return read_bundle_lightweight_at(bundle_dir);
    }

    with_shared_file_lock(&bundle_lock_target(bundle_dir), "task artifact v2", || {
        read_bundle_lightweight_at(bundle_dir)
    })
}

fn read_bundle_tolerating_in_flight(bundle_dir: &Path) -> Result<Option<TaskBundleV2>, OrbitError> {
    match read_bundle_lightweight_consistently(bundle_dir) {
        Ok(bundle) => Ok(Some(bundle)),
        Err(err) => skip_if_in_flight(bundle_dir, err),
    }
}

/// Convert a failed bundle read into `Ok(None)` when the bundle is provably
/// mid-flight rather than damaged, and re-raise it otherwise.
///
/// The directory is rechecked after the failed read: a concurrent deletion
/// can remove it between the registry snapshot and lock acquisition. An old
/// sentinel file alone is never evidence that a damaged bundle is in flight.
fn skip_if_in_flight<T>(bundle_dir: &Path, err: OrbitError) -> Result<Option<T>, OrbitError> {
    if bundle_dir.try_exists().unwrap_or(true) {
        return Err(err);
    }
    orbit_common::tracing::debug!(
        target: "orbit.store.task_bundle_v2",
        bundle_dir = %bundle_dir.display(),
        error = %err,
        "skipped a task bundle held by a concurrent writer",
    );
    Ok(None)
}
