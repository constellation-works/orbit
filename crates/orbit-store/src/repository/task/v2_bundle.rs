//! Task bundle v2 persistence is split into focused submodules while this root keeps store orchestration and re-exports.
//! The `task_bundle_types` submodule owns bundle value types and document filename mapping.
//! The `bundle_io` submodule owns bundle assembly, validation, JSONL repair, artifact manifest checks, and partial-bundle cleanup.
//! The `lock` submodule owns bundle create/delete lock ordering, sentinel cleanup, and projection-entry crash recovery checks.

use std::fs;
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

pub(crate) use crate::driver::file::task_bundle::{TaskBundleV2, TaskDocumentV2};
use crate::driver::file::task_bundle::{
    append_jsonl_row, cleanup_partial_bundle_best_effort, read_bundle_at, read_envelope_at,
    write_bundle_at,
};
use crate::driver::file::task_bundle::{
    remove_task_bundle_lock_sentinel, task_bundle_lock_sentinel_path,
};
use crate::driver::sqlite::task_registry::{
    ProjectionRebuildResult, TaskBundleBinding, TaskRegistryStore,
};
use crate::fs::yaml::write_yaml_atomic_with;
use crate::repository::checkout_projection::{
    ensure_projection_entry_removable, remove_projection_entry,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TaskBundleCreateResult {
    pub(crate) binding: TaskBundleBinding,
    pub(crate) projection: ProjectionRebuildResult,
}

pub(crate) struct TaskBundleStoreV2 {
    // pub(crate) fields widened to allow sibling `tests/v2_bundle.rs` (and promoted
    // `tests/test_support.rs`) to access internal state for durability and projection
    // assertions. See ORB-00247 and docs/design-patterns/test_layout.md (widen
    // deliberately rather than keep nested anti-pattern).
    #[cfg(test)]
    pub(crate) bundle_reads: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    pub(crate) envelope_reads: std::sync::atomic::AtomicUsize,
    pub(crate) registry: TaskRegistryStore,
    pub(crate) workspace_id: String,
    pub(crate) workspace_orbit_dir: Option<PathBuf>,
}

impl TaskBundleStoreV2 {
    pub(crate) fn new(
        registry: TaskRegistryStore,
        workspace_id: String,
        workspace_orbit_dir: PathBuf,
    ) -> Self {
        Self {
            #[cfg(test)]
            bundle_reads: Default::default(),
            #[cfg(test)]
            envelope_reads: Default::default(),
            registry,
            workspace_id,
            workspace_orbit_dir: Some(workspace_orbit_dir),
        }
    }

    pub(crate) fn new_checkoutless(registry: TaskRegistryStore, workspace_id: String) -> Self {
        Self {
            #[cfg(test)]
            bundle_reads: Default::default(),
            #[cfg(test)]
            envelope_reads: Default::default(),
            registry,
            workspace_id,
            workspace_orbit_dir: None,
        }
    }

    pub(crate) fn bundle_path(&self, task_id: &str) -> Result<PathBuf, OrbitError> {
        self.registry
            .canonical_task_bundle_path(&self.workspace_id, task_id)
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
            op,
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
        with_exclusive_file_lock(&path, "task action admission", || {
            // Under the *creation* lock, which does not exclude a lifecycle
            // write to an already-recovered bundle — so read this the same
            // coordinated way, or a replay racing a transition would declare a
            // perfectly good bundle unreadable.
            if let Ok(existing) = read_bundle_consistently(&path) {
                self.registry
                    .register_task_bundle(id, &self.workspace_id, &path)?;
                if let Some(workspace) = &self.workspace_orbit_dir
                    && let Err(error) = crate::repository::checkout_projection::rebuild_projection(
                        &self.registry,
                        workspace,
                        &self.workspace_id,
                    )
                {
                    orbit_common::tracing::warn!(task_id=id,error=%error,"recovered action task; checkout projection remains degraded");
                }
                remove_task_bundle_lock_sentinel(&task_bundle_lock_sentinel_path(&path)?)?;
                return Ok(existing);
            }
            if path.exists() {
                return Err(OrbitError::Store(
                    "action task bundle is unreadable; repair the retained bundle before replay"
                        .into(),
                ));
            }
            self.create_bundle_locked(id, &path, proposed)?;
            remove_task_bundle_lock_sentinel(&task_bundle_lock_sentinel_path(&path)?)?;
            Ok(proposed.clone())
        })
    }

    pub(crate) fn create_bundle(
        &self,
        bundle: &TaskBundleV2,
    ) -> Result<TaskBundleCreateResult, OrbitError> {
        let task_id = bundle.envelope.id.clone();
        let bundle_dir = self.bundle_path(&task_id)?;
        let lock_sentinel_path = task_bundle_lock_sentinel_path(&bundle_dir)?;
        with_exclusive_file_lock(&bundle_dir, "task bundle create", || {
            let result = self.create_bundle_locked(&task_id, &bundle_dir, bundle);
            match (
                result,
                remove_task_bundle_lock_sentinel(&lock_sentinel_path),
            ) {
                (Ok(created), Ok(())) => Ok(created),
                (Err(err), Ok(())) => Err(err),
                (Ok(_), Err(cleanup_err)) => Err(cleanup_err),
                (Err(err), Err(cleanup_err)) => {
                    orbit_common::tracing::warn!(
                        target: "orbit.store.task_bundle_v2",
                        task_id,
                        lock_path = %lock_sentinel_path.display(),
                        original_error = %err,
                        cleanup_error = %cleanup_err,
                        "failed to clean up task bundle lock sentinel",
                    );
                    Err(err)
                }
            }
        })
    }

    fn create_bundle_locked(
        &self,
        task_id: &str,
        bundle_dir: &Path,
        bundle: &TaskBundleV2,
    ) -> Result<TaskBundleCreateResult, OrbitError> {
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
        if let Err(err) =
            sync_parent_dir(bundle_dir).map_err(|err| OrbitError::from_write_io(bundle_dir, err))
        {
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
        let projection = match self.workspace_orbit_dir.as_deref() {
            None => ProjectionRebuildResult {
                projected: 0,
                repaired: 0,
                degraded_reason: None,
            },
            Some(workspace_orbit_dir) => {
                match crate::repository::checkout_projection::rebuild_projection(
                    &self.registry,
                    workspace_orbit_dir,
                    &self.workspace_id,
                ) {
                    Ok(projection) => projection,
                    Err(err) => {
                        orbit_common::tracing::warn!(
                            target: "orbit.store.task_bundle_v2",
                            task_id,
                            workspace_id = %self.workspace_id,
                            error = %err,
                            "task bundle created, but projection rebuild failed; continuing in degraded mode",
                        );
                        ProjectionRebuildResult {
                            projected: 0,
                            repaired: 0,
                            degraded_reason: Some(format!(
                                "projection rebuild failed after bundle creation: {err}"
                            )),
                        }
                    }
                }
            }
        };

        Ok(TaskBundleCreateResult {
            binding,
            projection,
        })
    }

    pub(crate) fn read_bundle(&self, task_id: &str) -> Result<TaskBundleV2, OrbitError> {
        #[cfg(test)]
        self.bundle_reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let bundle_dir = self.bundle_path(task_id)?;
        read_bundle_consistently(&bundle_dir)
    }

    pub(crate) fn delete_bundle(&self, task_id: &str) -> Result<bool, OrbitError> {
        orbit_types::task::validate_orb_task_id(task_id)?;
        let bundle_dir = self.bundle_path(task_id)?;

        if let Some(workspace_orbit_dir) = self.workspace_orbit_dir.as_deref() {
            ensure_projection_entry_removable(workspace_orbit_dir, task_id)?;
        }
        let unregistered = self
            .registry
            .unregister_task_bundle(task_id, &self.workspace_id)?;
        let removed_bundle = match fs::remove_dir_all(&bundle_dir) {
            Ok(()) => true,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => false,
            Err(err) => return Err(OrbitError::from_write_io(&bundle_dir, err)),
        };
        let removed_projection = match self.workspace_orbit_dir.as_deref() {
            Some(workspace_orbit_dir) => remove_projection_entry(workspace_orbit_dir, task_id)?,
            None => false,
        };
        Ok(unregistered || removed_bundle || removed_projection)
    }

    /// List bundles registered to this workspace.
    ///
    /// Corruption is still fail-fast — one damaged bundle fails the list so
    /// store damage is never silently hidden. What is *not* fail-fast is a
    /// bundle caught mid-publication or mid-removal by a concurrent writer
    /// (ORB-10988 / F2026-07-119): the binding list is a snapshot, so a create
    /// or delete of one task would otherwise fail every read of every other
    /// task. Those bundles are skipped, exactly as they would be had the
    /// snapshot been taken a moment earlier or later.
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

    /// Read one registered bundle, skipping it when a concurrent writer has it
    /// in flight. See [`Self::list_bundles`] for the tolerance rule.
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
        write_yaml_atomic_with(
            &self.bundle_path(task_id)?.join(TASK_ENVELOPE_FILE_NAME),
            envelope,
            |err| OrbitError::Store(err.to_string()),
        )
    }

    pub(crate) fn rewrite_artifact_manifest(
        &self,
        task_id: &str,
        manifest: &ArtifactManifestV2,
    ) -> Result<(), OrbitError> {
        manifest.validate()?;
        write_yaml_atomic_with(
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

/// The lock file coordinating one bundle's readers and writers.
///
/// Deliberately *not* the create/delete sentinel, which keys on the bundle
/// directory itself: a reader must not block a task's creation or removal, and
/// those transient states stay governed by [`skip_if_in_flight`].
fn bundle_lock_target(bundle_dir: &Path) -> PathBuf {
    bundle_dir.join(TASK_ENVELOPE_FILE_NAME)
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
    with_shared_file_lock(&bundle_lock_target(bundle_dir), "task artifact v2", || {
        read_bundle_at(bundle_dir)
    })
}

fn read_bundle_tolerating_in_flight(bundle_dir: &Path) -> Result<Option<TaskBundleV2>, OrbitError> {
    match read_bundle_consistently(bundle_dir) {
        Ok(bundle) => Ok(Some(bundle)),
        Err(err) => skip_if_in_flight(bundle_dir, err),
    }
}

/// Convert a failed bundle read into `Ok(None)` when the bundle is provably
/// mid-flight rather than damaged, and re-raise it otherwise.
///
/// Both discriminators are re-checked *after* the read failed, so a bundle
/// that read cleanly is never suppressed:
///
/// - the create/delete lock sentinel is present, meaning another writer holds
///   the bundle directory right now (or left the sentinel behind by deleting
///   the task, in which case there is no bundle to read anyway); or
/// - the bundle directory is gone, meaning the delete completed between the
///   registry snapshot and the read.
fn skip_if_in_flight<T>(bundle_dir: &Path, err: OrbitError) -> Result<Option<T>, OrbitError> {
    if !bundle_read_failure_is_in_flight(bundle_dir) {
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

fn bundle_read_failure_is_in_flight(bundle_dir: &Path) -> bool {
    if task_bundle_lock_sentinel_path(bundle_dir).is_ok_and(|path| path.exists()) {
        return true;
    }
    !bundle_dir.is_dir()
}
