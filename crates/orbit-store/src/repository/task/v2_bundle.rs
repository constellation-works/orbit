//! Task bundle orchestration joins file durability and registry bindings.
//! Full-bundle reads and mutations share a persistent external lock; deletion
//! publishes a recoverable rename before cleanup.

use std::fs::{self, File};
use std::path::{Path, PathBuf};

use orbit_common::OrbitError;
use orbit_common::fs::io::{
    FileLockOptions, FileLockProbe, HeldFileLock, atomic_write_text, atomic_write_text_volatile,
    hold_exclusive_file_lock_options, hold_step, probe_file_lock, sync_parent_dir,
    with_exclusive_file_lock, with_shared_file_lock,
};
use orbit_types::task::{
    ArtifactManifestV2, TASK_ARTIFACT_MANIFEST_FILE_NAME, TASK_ARTIFACTS_DIR_NAME,
    TASK_COMMENTS_FILE_NAME, TASK_ENVELOPE_FILE_NAME, TASK_EVENTS_FILE_NAME, TaskCommentRowV2,
    TaskEnvelopeV2, TaskEventRowV2,
};

use crate::driver::file::task_bundle::bundle_lock_target;
pub(crate) use crate::driver::file::task_bundle::{TaskBundleV2, TaskDocumentV2, TaskSearchDocs};
use crate::driver::file::task_bundle::{
    append_jsonl_row, append_jsonl_rows, cleanup_partial_bundle_best_effort, is_unpublished_stub,
    publish_envelope, read_bundle_at, read_bundle_lightweight_at, read_envelope_at,
    read_search_docs_at, recover_pending_write, write_bundle_at,
};
use crate::driver::sqlite::task_registry::{TaskBundleBinding, TaskRegistryStore};
use crate::fs::yaml::write_yaml_durable_with;

/// Steps a boundary section's hold report names for bundle reads
/// ([`hold_step`]): opening, reading and decoding the bundle's files. A wait
/// for the bundle's lock is a step of its own.
const BUNDLE_READ_STEP: &str = "bundle read";
const ENVELOPE_READ_STEP: &str = "envelope read";
const SEARCH_DOCS_READ_STEP: &str = "search docs read";

/// Directory beside a partition's bundles holding one lease per task whose
/// coordinated commit has prepared and not yet settled. Its committer holds
/// the lease's lock, inside the task's bundle lock, for the whole commit.
pub(crate) const COMMIT_LEASE_DIR: &str = ".task-commit-applying";

/// The commit lease for the task whose bundle is `bundle_dir`.
pub(crate) fn commit_lease_path(bundle_dir: &Path) -> Option<PathBuf> {
    Some(
        bundle_dir
            .parent()?
            .join(COMMIT_LEASE_DIR)
            .join(bundle_dir.file_name()?),
    )
}

/// Refuse a bundle whose coordinated commit died between its decision and
/// its apply. Its committer held the bundle lock for as long as it held the
/// lease, so a lease nobody holds, seen under that bundle lock, outlived it:
/// reading would show a half-applied transition, and writing would add rows
/// the commit's replay then truncates. The partition replays the commit the
/// next time any section enters it, so the caller can retry.
fn refuse_interrupted_commit(bundle_dir: &Path) -> Result<(), OrbitError> {
    let Some(lease) = commit_lease_path(bundle_dir) else {
        return Ok(());
    };
    match probe_file_lock(&lease).map_err(|err| OrbitError::from_write_io(&lease, err))? {
        FileLockProbe::Free => Err(OrbitError::Store(format!(
            "task bundle '{}' has an interrupted commit awaiting replay; retry once its \
             partition has recovered",
            bundle_dir.display()
        ))),
        FileLockProbe::Absent | FileLockProbe::Held => Ok(()),
    }
}

/// This task's exclusive bundle lock, held until the guard drops. See
/// [`TaskBundleStoreV2::hold_bundle_write_lock`].
pub(crate) struct BundleWriteHold {
    _lock: HeldFileLock,
}

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
    pub(crate) registry: TaskRegistryStore,
    pub(crate) workspace_id: String,
}

impl TaskBundleStoreV2 {
    pub(crate) fn new(registry: TaskRegistryStore, workspace_id: String) -> Self {
        Self {
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
    ///
    /// A leftover `.pending-write.yaml` is settled before `op` runs. Reads
    /// hide an aborted write's rows while the files still hold them, so a
    /// writer that skipped recovery would number new rows from the hidden
    /// view, append after the aborted ones, and republish the envelope —
    /// which recovery then takes as proof the aborted write committed.
    pub(crate) fn with_bundle_write_lock<T, F>(&self, task_id: &str, op: F) -> Result<T, OrbitError>
    where
        F: FnOnce() -> Result<T, OrbitError>,
    {
        let _hold = self.hold_bundle_write_lock(task_id)?;
        op()
    }

    /// [`Self::with_bundle_write_lock`]'s lock, held until the returned guard
    /// drops, for a coordinated commit that keeps the bundle after it
    /// releases its partition. A bundle whose earlier commit was interrupted
    /// is refused until its partition replays that commit.
    pub(crate) fn hold_bundle_write_lock(
        &self,
        task_id: &str,
    ) -> Result<BundleWriteHold, OrbitError> {
        let hold = self.hold_bundle_lock_for_write(task_id)?;
        if !hold._lock.is_reentry() {
            refuse_interrupted_commit(&self.bundle_path(task_id)?)?;
        }
        Ok(hold)
    }

    /// The write lock a commit replay takes: the one bundle writer admitted
    /// past an interrupted commit, since it is that commit's settlement.
    pub(crate) fn with_bundle_replay_lock<T, F>(
        &self,
        task_id: &str,
        op: F,
    ) -> Result<T, OrbitError>
    where
        F: FnOnce() -> Result<T, OrbitError>,
    {
        let _hold = self.hold_bundle_lock_for_write(task_id)?;
        op()
    }

    fn hold_bundle_lock_for_write(&self, task_id: &str) -> Result<BundleWriteHold, OrbitError> {
        let bundle_dir = self.bundle_path(task_id)?;
        let lock = hold_exclusive_file_lock_options(
            &bundle_lock_target(&bundle_dir),
            "task artifact v2",
            FileLockOptions::default(),
        )?;
        // A queued writer may resume after deletion. Check under the stable
        // lock before any helper can create parent directories.
        read_envelope_at(&bundle_dir)?;
        recover_pending_write(&bundle_dir)?;
        Ok(BundleWriteHold { _lock: lock })
    }

    /// Run `op` while holding this task's shared bundle lock.
    ///
    /// Holders exclude writers of this bundle but not each other. No
    /// pending-write recovery runs: that is a mutation, and the full-bundle
    /// read already hides an aborted write's rows.
    pub(crate) fn with_bundle_read_lock<T, F>(&self, task_id: &str, op: F) -> Result<T, OrbitError>
    where
        F: FnOnce() -> Result<T, OrbitError>,
    {
        let bundle_dir = self.bundle_path(task_id)?;
        with_shared_file_lock(&bundle_lock_target(&bundle_dir), "task artifact v2", || {
            read_envelope_at(&bundle_dir)?;
            refuse_interrupted_commit(&bundle_dir)?;
            op()
        })
    }

    /// The caller has durably reserved this ID for one action and input digest.
    /// Re-enter after a crash under the canonical bundle lock. A readable bundle
    /// wins; unreadable bytes are retained for explicit recovery.
    /// The replay flag is decided under the lock, so concurrent creators
    /// cannot both report publishing the same bundle.
    pub(crate) fn create_or_recover_action_bundle(
        &self,
        proposed: &TaskBundleV2,
    ) -> Result<(TaskBundleV2, bool), OrbitError> {
        let id = &proposed.envelope.id;
        let path = self.bundle_path(id)?;
        with_exclusive_file_lock(&bundle_lock_target(&path), "task action admission", || {
            // Re-entrant read under the same lock as lifecycle writes.
            if let Ok(existing) = read_bundle_consistently(&path) {
                self.registry
                    .register_task_bundle(id, &self.workspace_id, &path)?;
                return Ok((existing, true));
            }
            if path.exists() {
                return Err(OrbitError::Store(
                    "action task bundle is unreadable; repair the retained bundle before replay"
                        .into(),
                ));
            }
            self.create_bundle_locked(id, &path, proposed)?;
            Ok((proposed.clone(), false))
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
        let bundle_dir = self.bundle_path(task_id)?;
        read_bundle_consistently(&bundle_dir)
    }

    /// Registration is the creation commit point, after the complete bundle and
    /// its parent directory have been synced. Check it under the publication /
    /// deletion lock, then verify only the envelope identity. Mutable documents,
    /// history, comments and artifacts are deliberately outside receipt recovery.
    pub(crate) fn committed_creation_id(
        &self,
        task_id: &str,
    ) -> Result<Option<String>, OrbitError> {
        let bundle_dir = self.bundle_path(task_id)?;
        with_shared_file_lock(
            &bundle_lock_target(&bundle_dir),
            "task creation receipt",
            || {
                let Some(binding) = self.registry.find_task_binding(task_id)? else {
                    // The existing creation path will either finish publication at
                    // the reserved ID or refuse an unreadable retained bundle.
                    return Ok(None);
                };
                if binding.partition_id != self.workspace_id
                    || crate::fs::path_safety::normalize_path(&binding.canonical_path)
                        != crate::fs::path_safety::normalize_path(&bundle_dir)
                {
                    return Err(OrbitError::Store(
                        "creation task registration does not match its workspace and bundle".into(),
                    ));
                }
                // Do not translate missing/corrupt registered envelopes to None:
                // that would route damage into the creation/recovery path.
                let envelope = read_envelope_at(&bundle_dir)?;
                envelope
                    .validate()
                    .map_err(|error| OrbitError::Store(error.to_string()))?;
                Ok(Some(envelope.id))
            },
        )
    }

    /// Assemble a task bundle without opening artifact payload bytes.
    pub(crate) fn read_bundle_lightweight(
        &self,
        task_id: &str,
    ) -> Result<TaskBundleV2, OrbitError> {
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
        if exists {
            fs::rename(bundle_dir, &tombstone)
                .map_err(|err| OrbitError::from_write_io(bundle_dir, err))?;
        }
        if exists || published {
            sync_parent_path(&tombstone)?;
        }
        let unregistered = self
            .registry
            .unregister_task_bundle(task_id, &self.workspace_id)?;
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
        read_bundle_tolerating_in_flight(&self.bundle_path(task_id)?)
    }

    /// The documents task search matches on, read under this bundle's shared
    /// lock without its envelope or event log (see [`read_search_docs_at`]).
    ///
    /// A pruning read: `None` means the files cannot decide (a pending write,
    /// a bundle in flight, any read failure), and the caller falls back to
    /// [`Self::read_bundle_if_settled`], which decides authoritatively and
    /// reports the failure itself.
    pub(crate) fn read_search_docs(&self, task_id: &str) -> Option<TaskSearchDocs> {
        let bundle_dir = self.bundle_path(task_id).ok()?;
        if !bundle_dir.try_exists().ok()? {
            return None;
        }
        hold_step(SEARCH_DOCS_READ_STEP, || {
            with_shared_file_lock(&bundle_lock_target(&bundle_dir), "task artifact v2", || {
                read_search_docs_at(&bundle_dir)
            })
        })
        .ok()
        .flatten()
    }

    /// Read one registered bundle's envelope, skipping it when a concurrent
    /// writer has it in flight. See [`Self::list_bundles`] for the rule.
    pub(crate) fn read_envelope_if_settled(
        &self,
        task_id: &str,
    ) -> Result<Option<TaskEnvelopeV2>, OrbitError> {
        let bundle_dir = self.bundle_path(task_id)?;
        match hold_step(ENVELOPE_READ_STEP, || read_envelope_at(&bundle_dir)) {
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
        validate_envelope_for(task_id, envelope)?;
        publish_envelope(&self.envelope_path(task_id)?, envelope)
    }

    /// Show `envelope` to readers at once, without its durability syncs. For
    /// a coordinated commit's decided transition, whose journal replays it
    /// durably if the volatile copy is lost; [`Self::rewrite_envelope`]
    /// makes it durable before the journal settles.
    pub(crate) fn publish_envelope_volatile(
        &self,
        task_id: &str,
        envelope: &TaskEnvelopeV2,
    ) -> Result<(), OrbitError> {
        validate_envelope_for(task_id, envelope)?;
        let path = self.envelope_path(task_id)?;
        let yaml = crate::fs::yaml::serialize_yaml_with(envelope, |err| {
            OrbitError::Store(err.to_string())
        })?;
        atomic_write_text_volatile(&path, &yaml)
            .map_err(|err| OrbitError::from_write_io(&path, err))
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

    /// Append `events` in order with one write and one sync.
    pub(crate) fn append_events(
        &self,
        task_id: &str,
        events: &[TaskEventRowV2],
    ) -> Result<(), OrbitError> {
        for event in events {
            event.validate()?;
        }
        append_jsonl_rows(
            &self.bundle_path(task_id)?.join(TASK_EVENTS_FILE_NAME),
            events,
        )
    }

    /// Append `comments` in order with one write and one sync.
    pub(crate) fn append_comments(
        &self,
        task_id: &str,
        comments: &[TaskCommentRowV2],
    ) -> Result<(), OrbitError> {
        for comment in comments {
            comment.validate()?;
        }
        append_jsonl_rows(
            &self.bundle_path(task_id)?.join(TASK_COMMENTS_FILE_NAME),
            comments,
        )
    }
}

/// A published deletion is retained here until registry/projection removal and
/// cleanup finish. Canonical and tombstone coexistence is ambiguous and retained.
pub(crate) fn deletion_path(bundle_dir: &Path) -> PathBuf {
    bundle_dir.with_extension("deleted")
}

fn validate_envelope_for(task_id: &str, envelope: &TaskEnvelopeV2) -> Result<(), OrbitError> {
    if envelope.id != task_id {
        return Err(OrbitError::InvalidInput(format!(
            "task envelope id '{}' does not match target task id '{task_id}'",
            envelope.id
        )));
    }
    Ok(envelope.validate()?)
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

    #[cfg(test)]
    CANONICAL_BUNDLE_READS.with(|reads| reads.set(reads.get() + 1));
    hold_step(BUNDLE_READ_STEP, || {
        with_shared_file_lock(&bundle_lock_target(bundle_dir), "task artifact v2", || {
            refuse_interrupted_commit(bundle_dir)?;
            read_bundle_at(bundle_dir)
        })
    })
}

/// Same lock as [`read_bundle_consistently`], without hashing artifact blobs.
fn read_bundle_lightweight_consistently(bundle_dir: &Path) -> Result<TaskBundleV2, OrbitError> {
    #[cfg(test)]
    LIGHTWEIGHT_BUNDLE_READS.with(|reads| reads.set(reads.get() + 1));
    if !bundle_dir.try_exists()? {
        return read_bundle_lightweight_at(bundle_dir);
    }

    hold_step(BUNDLE_READ_STEP, || {
        with_shared_file_lock(&bundle_lock_target(bundle_dir), "task artifact v2", || {
            refuse_interrupted_commit(bundle_dir)?;
            read_bundle_lightweight_at(bundle_dir)
        })
    })
}

#[cfg(test)]
thread_local! {
    /// Listing-path bundle reads on this thread, so tests can bound what a
    /// read costs while the generated index is degraded.
    pub(crate) static LISTING_BUNDLE_READS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// Every lightweight bundle read on this thread, listing or by id, so
    /// tests can bound how many bundles an admission section opens.
    pub(crate) static LIGHTWEIGHT_BUNDLE_READS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// Canonical (artifact-hashing) bundle reads on this thread.
    pub(crate) static CANONICAL_BUNDLE_READS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

fn read_bundle_tolerating_in_flight(bundle_dir: &Path) -> Result<Option<TaskBundleV2>, OrbitError> {
    #[cfg(test)]
    LISTING_BUNDLE_READS.with(|reads| reads.set(reads.get() + 1));
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
/// Unpublished stubs (no `task.yaml` and no bundle content) are skipped so
/// listing of healthy neighbors still succeeds. A data-bearing directory
/// missing `task.yaml` is corruption, not a stub.
fn skip_if_in_flight<T>(bundle_dir: &Path, err: OrbitError) -> Result<Option<T>, OrbitError> {
    if is_unpublished_stub(bundle_dir) {
        orbit_common::tracing::debug!(
            target: "orbit.store.task_bundle_v2",
            bundle_dir = %bundle_dir.display(),
            error = %err,
            "skipped an unpublished task-bundle stub",
        );
        return Ok(None);
    }
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
