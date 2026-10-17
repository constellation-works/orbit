//! A decided commit's apply, after its admission section: the commit lease
//! that marks the commit in flight, and the per-thread queue of applies an
//! outermost admission section runs once it has released the partition.

use super::{BoundaryDepth, TaskCommitBoundary, TaskCommitIntent};
use crate::repository::task::v2_bundle::{BundleWriteHold, COMMIT_LEASE_DIR, commit_lease_path};
use orbit_common::OrbitError;
use orbit_common::fs::io::{FileLockProbe, LeaseFile, probe_file_lock, remove_path_if_exists};
use std::cell::RefCell;
use std::path::{Path, PathBuf};

/// A decided commit whose bundle apply is still owed, with the task's bundle
/// lock and commit lease its committer has held since before it prepared.
///
/// Fields drop in order, so an apply that never runs unlocks its lease,
/// leaving it dead, before it releases the bundle: no writer gets the bundle
/// while its lease still reads as live.
pub(super) struct DeferredApply {
    pub(super) journal_id: String,
    pub(super) intent: TaskCommitIntent,
    pub(super) lease: LeaseFile,
    pub(super) bundle: BundleWriteHold,
}

struct ApplyFrame {
    partition: PathBuf,
    applies: Vec<DeferredApply>,
}

thread_local! {
    /// The applies each outermost admission section on this thread owes,
    /// one frame per partition.
    static DEFERRED: RefCell<Vec<ApplyFrame>> = const { RefCell::new(Vec::new()) };
}

fn take_frame(partition: &Path) -> Vec<DeferredApply> {
    DEFERRED.with(|frames| {
        let mut frames = frames.borrow_mut();
        frames
            .iter()
            .rposition(|frame| frame.partition == partition)
            .map(|index| frames.remove(index).applies)
            .unwrap_or_default()
    })
}

/// The outermost admission section's queue of applies for its partition.
/// A nested section shares the outer one's.
pub(super) struct ApplyScope {
    partition: Option<PathBuf>,
}

impl ApplyScope {
    pub(super) fn open(partition: &Path) -> Self {
        DEFERRED.with(|frames| {
            let mut frames = frames.borrow_mut();
            if frames.iter().any(|frame| frame.partition == partition) {
                return Self { partition: None };
            }
            frames.push(ApplyFrame {
                partition: partition.to_path_buf(),
                applies: Vec::new(),
            });
            Self {
                partition: Some(partition.to_path_buf()),
            }
        })
    }

    /// Run the queued applies, once the section has released its locks, and
    /// return the section's result unless an apply failed. A failed apply
    /// leaves its lease dead, so the partition replays it on its next entry.
    pub(super) fn finish<T>(
        mut self,
        boundary: &TaskCommitBoundary,
        result: Result<T, OrbitError>,
    ) -> Result<T, OrbitError> {
        let Some(partition) = self.partition.take() else {
            return result;
        };
        let mut failed = None;
        for apply in take_frame(&partition) {
            if let Err(error) = boundary.apply_deferred(apply) {
                failed.get_or_insert(error);
            }
        }
        match failed {
            Some(error) if result.is_ok() => Err(error),
            _ => result,
        }
    }
}

impl Drop for ApplyScope {
    /// An unwinding section abandons its applies; their leases are left dead.
    fn drop(&mut self) {
        if let Some(partition) = self.partition.take() {
            drop(take_frame(&partition));
        }
    }
}

impl TaskCommitBoundary {
    /// Hand a decided commit to this thread's outermost admission section, or
    /// apply it now outside one.
    pub(super) fn defer_apply(&self, apply: DeferredApply) -> Result<(), OrbitError> {
        let unqueued = DEFERRED.with(|frames| {
            let mut frames = frames.borrow_mut();
            match frames
                .iter_mut()
                .rev()
                .find(|frame| frame.partition == self.partition_dir)
            {
                Some(frame) => {
                    frame.applies.push(apply);
                    None
                }
                None => Some(apply),
            }
        });
        unqueued.map_or(Ok(()), |apply| self.apply_deferred(apply))
    }

    /// Apply this thread's queued commit of `task_id`, so a later commit of
    /// the task in the same section builds on its bundle.
    pub(super) fn flush_deferred_apply(&self, task_id: &str) -> Result<(), OrbitError> {
        let queued = DEFERRED.with(|frames| {
            let mut frames = frames.borrow_mut();
            let frame = frames
                .iter_mut()
                .rev()
                .find(|frame| frame.partition == self.partition_dir)?;
            let index = frame
                .applies
                .iter()
                .position(|apply| apply.intent.task_id == task_id)?;
            Some(frame.applies.remove(index))
        });
        queued.map_or(Ok(()), |apply| self.apply_deferred(apply))
    }

    fn apply_deferred(&self, apply: DeferredApply) -> Result<(), OrbitError> {
        #[cfg(test)]
        if let Some(fault) = apply_fault::take() {
            return self.crash_apply(apply, fault);
        }
        #[cfg(test)]
        slow_apply::run();
        // A failed apply drops `apply` whole, leaving its lease dead.
        self.apply_committed(&apply.journal_id, &apply.intent)?;
        let DeferredApply { lease, bundle, .. } = apply;
        let path = lease.path().to_path_buf();
        if let Err(error) = lease.release() {
            // The commit is complete; its unnamed-but-free lease only makes
            // the next section replay an applied row, which settles nothing.
            orbit_common::tracing::warn!(
                target: "orbit.store.task_commit",
                lease = %path.display(),
                error = %error,
                "an applied task commit could not remove its commit lease",
            );
        }
        drop(bundle);
        Ok(())
    }

    /// Simulate a crash at `fault`: the apply stops there and its locks go
    /// as a dead process's would.
    #[cfg(test)]
    fn crash_apply(
        &self,
        apply: DeferredApply,
        fault: apply_fault::Fault,
    ) -> Result<(), OrbitError> {
        if fault == apply_fault::Fault::AfterEvents {
            self.apply_events(&apply.intent)?;
        }
        drop(apply);
        Err(OrbitError::Store(
            "simulated crash before the commit finished applying".into(),
        ))
    }

    /// Name this task's commit lease, locked by this process, before its
    /// commit prepares.
    pub(super) fn take_commit_lease(&self, task_id: &str) -> Result<LeaseFile, OrbitError> {
        let path = self.commit_lease(task_id)?;
        LeaseFile::acquire(&path).map_err(|error| OrbitError::from_write_io(&path, error))
    }

    pub(super) fn commit_lease(&self, task_id: &str) -> Result<PathBuf, OrbitError> {
        commit_lease_path(&self.bundle_store.bundle_path(task_id)?).ok_or_else(|| {
            OrbitError::Store(format!(
                "task '{task_id}' has no partition for its commit lease"
            ))
        })
    }

    pub(super) fn release_commit_lease(lease: LeaseFile) -> Result<(), OrbitError> {
        let path = lease.path().to_path_buf();
        lease
            .release()
            .map_err(|error| OrbitError::from_write_io(&path, error))
    }

    /// Replay any unsettled commit no live committer holds the lease of.
    ///
    /// A lease is not durable, so a host crash can lose one whose decision
    /// survived, and then nothing in the partition says the commit is owed.
    /// Every process opens its boundary before it reads the partition, so
    /// checking the journal here settles such a commit after the restart.
    pub(super) fn recover_unowned_commits(&self) -> Result<(), OrbitError> {
        if BoundaryDepth::active(&self.partition_dir) {
            return Ok(());
        }
        for record in self
            .store
            .unsettled_task_commit_journal(&self.workspace_id)?
        {
            if probe_file_lock(&self.commit_lease(&record.task_id)?)? != FileLockProbe::Held {
                return self.recover();
            }
        }
        Ok(())
    }

    /// Whether a commit in this partition was interrupted: a lease its dead
    /// committer left, or the pending marker older versions held for the
    /// whole of a commit.
    pub(super) fn interrupted_commit(&self) -> Result<bool, OrbitError> {
        Ok(self.dead_commit_lease()? || self.pending_marker_path().try_exists()?)
    }

    /// Whether any task's commit lease outlived its committer. Staged leases
    /// (dot-prefixed) never carry a decision; recovery clears them.
    pub(super) fn dead_commit_lease(&self) -> Result<bool, OrbitError> {
        for path in self.commit_leases()? {
            let staged = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_none_or(|name| name.starts_with('.'));
            if !staged && probe_file_lock(&path)? == FileLockProbe::Free {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Remove every lease nobody holds. Caller holds the partition
    /// exclusively, so no committer is naming a new one.
    pub(super) fn clear_dead_commit_leases(&self) -> Result<(), OrbitError> {
        for path in self.commit_leases()? {
            if probe_file_lock(&path)? == FileLockProbe::Free {
                remove_path_if_exists(&path)
                    .map_err(|error| OrbitError::from_write_io(&path, error))?;
            }
        }
        Ok(())
    }

    fn commit_leases(&self) -> Result<Vec<PathBuf>, OrbitError> {
        let dir = self.partition_dir.join(COMMIT_LEASE_DIR);
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(OrbitError::from_write_io(&dir, error)),
        };
        entries
            .map(|entry| {
                entry
                    .map(|entry| entry.path())
                    .map_err(|error| OrbitError::from_write_io(&dir, error))
            })
            .collect()
    }
}

/// A hook run at the start of a commit's apply, so a test can hold the apply
/// until a concurrent caller proves what it waits for.
#[cfg(test)]
pub(crate) mod slow_apply {
    use std::cell::RefCell;

    thread_local! {
        static HOOK: RefCell<Option<Box<dyn FnOnce()>>> = const { RefCell::new(None) };
    }

    /// Run `hook` once, in the next commit applied on this thread.
    pub(crate) fn set(hook: impl FnOnce() + 'static) {
        HOOK.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
    }

    pub(super) fn run() {
        if let Some(hook) = HOOK.with(|slot| slot.borrow_mut().take()) {
            hook();
        }
    }
}

/// A crash injected into the next commit applied on this thread.
#[cfg(test)]
pub(crate) mod apply_fault {
    use std::cell::Cell;

    /// How far the crashed apply got.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum Fault {
        /// Decided, nothing applied.
        BeforeApply,
        /// The history appended, nothing else.
        AfterEvents,
    }

    thread_local! {
        static FAULT: Cell<Option<Fault>> = const { Cell::new(None) };
    }

    pub(crate) fn set(fault: Fault) {
        FAULT.with(|slot| slot.set(Some(fault)));
    }

    pub(super) fn take() -> Option<Fault> {
        FAULT.with(Cell::take)
    }
}
