//! The recoverable task/reservation commit boundary.
//!
//! # Why this exists
//!
//! A task lives in three places that cannot commit together: its bundle files,
//! the registry index, and the reservation database. Admission has to publish a
//! status transition, its history, and a reservation as *one* fact, and it has
//! to read readiness under the same serialization that ordinary task and
//! reservation writes take — otherwise a task can be admitted from state that
//! changed between the check and the write.
//!
//! # The protocol
//!
//! 1. **Serialize.** Every participant enters one advisory lock per task-store
//!    partition. Ordinary task reads/writes and ordinary reservation writes
//!    take it *shared*, so they keep running concurrently with each other; an
//!    admission section takes it *exclusive*. The lock is re-entrant per
//!    thread ([`orbit_common::fs::io`]), so a nested task lock inside an
//!    admission section runs under the outer acquisition instead of
//!    self-deadlocking. Every participant takes the boundary before any bundle
//!    lock, so the two locks are always acquired in the same order.
//! 2. **Prepare.** Write a durable pending marker in the partition directory,
//!    then insert a `prepared` journal row. Nothing about the task has changed
//!    yet.
//! 3. **Decide.** One SQLite transaction inserts the reservation and dependent
//!    coordination rows and flips the journal row to `committed`. This is the
//!    commit point and the only place the decision exists.
//! 4. **Apply.** Roll the decision forward onto the bundle: truncate
//!    `events.jsonl` to its recorded pre-apply length, append the intent's
//!    events, republish `task.yaml`, settle the journal row `applied`, and
//!    remove the marker.
//!
//! A failure before step 3 leaves no reservation, no coordination row, and an
//! untouched bundle: compensation only has to abandon an undecided row. A
//! failure after step 3 leaves a durable decision that step 4 replays
//! verbatim, as many times as it takes. Nothing consults the bundle to infer
//! whether a commit happened, and nothing tries to un-publish a published
//! envelope.
//!
//! # One composition per partition
//!
//! The boundary serializes what holds the same instance, so a partition must
//! be served by *one* composition: coordinated
//! ([`crate::compose::workspace_coordinated_backends`]) or uncoordinated, not
//! both at once. An uncoordinated writer neither takes the boundary nor sees
//! the pending marker, so it could write a bundle between a commit and its
//! replay and have the replay overwrite it.
//!
//! # Recovery before exposure
//!
//! The pending marker is the cheap signal that a commit is in flight or was
//! interrupted. Any participant that sees it takes the boundary exclusively
//! and replays the journal before it reads or writes anything, so no caller
//! observes a committed reservation whose task transition has not landed. A
//! compensation or replay that fails leaves the marker in place and returns
//! the error: the partition stays closed until recovery succeeds.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::fs::io::{
    atomic_write_text, create_private_dir_all, with_exclusive_file_lock, with_shared_file_lock,
};
use orbit_types::task::{
    TASK_ARTIFACT_SCHEMA_VERSION, TASK_EVENTS_FILE_NAME, TaskEnvelopeV2, TaskEventRowV2,
};
use serde::{Deserialize, Serialize};

use crate::Store;
use crate::contracts::{
    TaskCommitJournalState, TaskCoordinationCommit, TaskCoordinationCommitOutcome,
    TaskCoordinationCommitParams,
};
use crate::driver::file::task_bundle::truncate_jsonl_file;
use crate::driver::sqlite::task_commit_journal::JournalCommitOutcome;
use crate::driver::sqlite::task_registry::TaskRegistryStore;
use crate::repository::task::v2::sequencing::next_sequence;
use crate::repository::task::v2_bundle::{TaskBundleStoreV2, TaskBundleV2};

/// Lock target for one task-store partition. The advisory lock is its
/// dot-prefixed sibling, so this name is never itself created or removed.
const COORDINATION_LOCK_FILE: &str = "task-commit";
const COORDINATION_LOCK_LABEL: &str = "task commit boundary";
/// Present from the first durable step of a commit until the commit has been
/// applied or abandoned.
const PENDING_MARKER_FILE: &str = ".task-commit-pending";
const COMMIT_INTENT_SCHEMA_VERSION: u32 = 1;

static JOURNAL_ID_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Injection points for the durability tests. Each names a moment the process
/// can die and a recovery obligation that follows from it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum CoordinationFault {
    /// After the marker is durable, before the journal row exists.
    AfterMarker,
    /// After the undecided journal row is durable, before the commit point.
    BeforeCommit,
    /// After the commit point, before any bundle file changes.
    AfterCommit,
    /// Midway through rolling a committed decision onto the bundle.
    DuringApply,
    /// While abandoning an undecided commit.
    DuringCompensation,
    /// While replaying the journal on the next entry.
    DuringRecovery,
}

#[cfg(test)]
thread_local! {
    static INJECTED_FAULTS: std::cell::RefCell<std::collections::HashSet<CoordinationFault>> =
        std::cell::RefCell::new(std::collections::HashSet::new());
}

#[cfg(test)]
pub(crate) fn inject_coordination_faults(faults: &[CoordinationFault]) {
    INJECTED_FAULTS.with(|cell| {
        *cell.borrow_mut() = faults.iter().copied().collect();
    });
}

fn fail_if_injected(_fault: CoordinationFault) -> Result<(), OrbitError> {
    #[cfg(test)]
    {
        let hit = INJECTED_FAULTS.with(|cell| cell.borrow_mut().remove(&_fault));
        if hit {
            return Err(OrbitError::Store(format!(
                "injected coordination failure at {_fault:?}"
            )));
        }
    }
    Ok(())
}

thread_local! {
    /// Depth of boundary sections this thread is inside. Recovery must never
    /// run *inside* a commit this thread is performing: that commit's own
    /// journal row is legitimately unsettled.
    static BOUNDARY_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

struct BoundaryDepth;

impl BoundaryDepth {
    fn enter() -> Self {
        BOUNDARY_DEPTH.with(|depth| depth.set(depth.get() + 1));
        Self
    }

    fn active() -> bool {
        BOUNDARY_DEPTH.with(|depth| depth.get() > 0)
    }
}

impl Drop for BoundaryDepth {
    fn drop(&mut self) {
        BOUNDARY_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

/// The bundle-side half of a commit, replayed verbatim by recovery.
///
/// `events_len` is the length of `events.jsonl` before the apply, so a replay
/// after a partial append is deterministic: truncate, then append exactly
/// these rows.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TaskCommitIntent {
    schema_version: u32,
    task_id: String,
    events_len: u64,
    events: Vec<TaskEventRowV2>,
    envelope: TaskEnvelopeV2,
}

/// One task-store partition's durable commit and recovery authority.
///
/// Construct it once per partition and share it: the boundary is only a
/// boundary if every participant holds the same one. See the module docs for
/// the protocol and [`TaskCommitBoundary::commit_task_transition`] for the
/// integration entry point.
pub struct TaskCommitBoundary {
    store: Store,
    registry: TaskRegistryStore,
    bundle_store: TaskBundleStoreV2,
    workspace_id: String,
    partition_dir: PathBuf,
}

impl TaskCommitBoundary {
    pub fn new(
        store: Store,
        registry: TaskRegistryStore,
        workspace_id: String,
    ) -> Result<Self, OrbitError> {
        let partition_dir = registry.workspace_partition_dir(&workspace_id)?;
        create_private_dir_all(&partition_dir)
            .map_err(|error| OrbitError::from_write_io(&partition_dir, error))?;
        Ok(Self {
            bundle_store: TaskBundleStoreV2::new(registry.clone(), workspace_id.clone()),
            store,
            registry,
            workspace_id,
            partition_dir,
        })
    }

    /// The task-store partition this boundary serializes.
    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
    }

    /// Run an ordinary task or reservation operation inside the boundary.
    ///
    /// Shared with every other ordinary participant and excluded by an
    /// admission section. Reads take it too, so a caller cannot observe a
    /// reservation whose task transition is still being applied.
    pub fn enter_ordinary<T, F>(&self, op: F) -> Result<T, OrbitError>
    where
        F: FnOnce() -> Result<T, OrbitError>,
    {
        self.recover_if_pending()?;
        let _depth = BoundaryDepth::enter();
        with_shared_file_lock(&self.lock_target(), COORDINATION_LOCK_LABEL, op)
    }

    /// Hold the boundary exclusively for one admission decision.
    ///
    /// Readiness, dependencies, conflicts, and the commit itself run inside
    /// `op`, so nothing an ordinary write could change moves underneath the
    /// decision. Calling [`Self::commit_task_transition`] inside `op` re-enters
    /// the same acquisition.
    pub fn with_admission<T, F>(&self, op: F) -> Result<T, OrbitError>
    where
        F: FnOnce() -> Result<T, OrbitError>,
    {
        self.recover_if_pending()?;
        let _depth = BoundaryDepth::enter();
        with_exclusive_file_lock(&self.lock_target(), COORDINATION_LOCK_LABEL, op)
    }

    /// Publish a task transition and history together with an optional
    /// reservation and dependent coordination rows.
    ///
    /// The whole call runs in an admission section, so it is safe to call
    /// directly or from inside [`Self::with_admission`].
    pub fn commit_task_transition(
        &self,
        params: &TaskCoordinationCommitParams,
    ) -> Result<TaskCoordinationCommitOutcome, OrbitError> {
        self.with_admission(|| self.commit_locked(params))
    }

    /// Dependent coordination rows published for this partition, by kind.
    ///
    /// Settles an interrupted commit first, so a caller reading back its own
    /// receipts cannot miss one that a crashed commit had already decided.
    pub fn coordination_rows(
        &self,
        kind: &str,
    ) -> Result<Vec<crate::contracts::TaskCoordinationRow>, OrbitError> {
        self.recover_if_pending()?;
        self.store.task_coordination_rows(&self.workspace_id, kind)
    }

    /// Replay the journal when a marker says a commit may be unfinished.
    ///
    /// Cheap on the common path: one existence check.
    pub fn recover_if_pending(&self) -> Result<(), OrbitError> {
        if BoundaryDepth::active() {
            // This thread is inside its own commit; its journal row is
            // unsettled on purpose.
            return Ok(());
        }
        if !self.pending_marker_path().try_exists()? {
            return Ok(());
        }
        self.recover()
    }

    /// Settle every unfinished commit for this partition: roll undecided
    /// intents back, roll committed decisions forward.
    pub fn recover(&self) -> Result<(), OrbitError> {
        with_exclusive_file_lock(&self.lock_target(), COORDINATION_LOCK_LABEL, || {
            let _depth = BoundaryDepth::enter();
            fail_if_injected(CoordinationFault::DuringRecovery)?;
            for record in self
                .store
                .unsettled_task_commit_journal(&self.workspace_id)?
            {
                match record.state {
                    TaskCommitJournalState::Prepared => {
                        self.store.abort_task_commit_journal(&record.journal_id)?;
                    }
                    TaskCommitJournalState::Committed => {
                        let intent: TaskCommitIntent = serde_json::from_str(&record.intent_json)
                            .map_err(|error| {
                                OrbitError::Store(format!(
                                    "task commit journal '{}' carries an unreadable intent: {error}",
                                    record.journal_id
                                ))
                            })?;
                        self.bundle_store
                            .with_bundle_write_lock(&intent.task_id, || {
                                self.apply_committed(&record.journal_id, &intent)
                            })?;
                    }
                    TaskCommitJournalState::Applied | TaskCommitJournalState::Aborted => {}
                }
            }
            self.clear_pending_marker()
        })
    }

    fn commit_locked(
        &self,
        params: &TaskCoordinationCommitParams,
    ) -> Result<TaskCoordinationCommitOutcome, OrbitError> {
        orbit_types::task::validate_orb_task_id(&params.task_id)?;
        if params.actor.trim().is_empty() {
            return Err(OrbitError::InvalidInput(
                "task coordination actor must not be empty".to_string(),
            ));
        }

        self.bundle_store
            .with_bundle_write_lock(&params.task_id, || {
                // The intent needs the envelope and the event log, not artifact
                // payload bytes; task-field corruption still fails this read.
                let bundle = self.bundle_store.read_bundle_lightweight(&params.task_id)?;
                let current_status = bundle.envelope.status;
                if !params.expected_status.is_empty()
                    && !params.expected_status.contains(&current_status)
                {
                    return Ok(TaskCoordinationCommitOutcome::Stale { current_status });
                }

                let intent = self.build_intent(&bundle, params)?;
                let intent_json = serde_json::to_string(&intent)
                    .map_err(|error| OrbitError::Store(error.to_string()))?;
                let journal_id = unique_journal_id();

                self.write_pending_marker(&journal_id)?;
                fail_if_injected(CoordinationFault::AfterMarker)?;
                self.store.prepare_task_commit_journal(
                    &journal_id,
                    &self.workspace_id,
                    &params.task_id,
                    &intent_json,
                )?;

                if let Err(error) = fail_if_injected(CoordinationFault::BeforeCommit) {
                    self.compensate_prepared(&journal_id)?;
                    return Err(error);
                }

                let decided = match self.store.commit_task_commit_journal(
                    &journal_id,
                    params.reservation.as_ref(),
                    &params.rows,
                ) {
                    Ok(decided) => decided,
                    Err(error) => {
                        self.compensate_prepared(&journal_id)?;
                        return Err(error);
                    }
                };

                match decided {
                    JournalCommitOutcome::Conflicted(result) => {
                        self.compensate_prepared(&journal_id)?;
                        Ok(TaskCoordinationCommitOutcome::Conflicted {
                            conflicts: result.conflicts,
                            expired_reservations: result.expired_reservations,
                        })
                    }
                    JournalCommitOutcome::RowExists { kind, row_id } => {
                        self.compensate_prepared(&journal_id)?;
                        Ok(TaskCoordinationCommitOutcome::RowExists { kind, row_id })
                    }
                    JournalCommitOutcome::Committed(reservation) => {
                        // Past the commit point: the decision is durable and the
                        // bundle apply is a replay obligation, never a rollback.
                        fail_if_injected(CoordinationFault::AfterCommit)?;
                        self.apply_committed(&journal_id, &intent)?;
                        Ok(TaskCoordinationCommitOutcome::Committed(
                            TaskCoordinationCommit {
                                journal_id,
                                task_id: params.task_id.clone(),
                                status: intent.envelope.status,
                                reservation,
                                rows: params.rows.clone(),
                            },
                        ))
                    }
                }
            })
    }

    /// Derive the bundle-side half of the commit from the state read under
    /// the boundary.
    fn build_intent(
        &self,
        bundle: &TaskBundleV2,
        params: &TaskCoordinationCommitParams,
    ) -> Result<TaskCommitIntent, OrbitError> {
        let now = Utc::now();
        let current_status = bundle.envelope.status;
        let target_status = params.status.unwrap_or(current_status);
        let transition =
            (target_status != current_status).then_some((current_status, target_status));

        let mut next_event = next_sequence(&bundle.events, "EV-");
        let mut events = Vec::new();
        for entry in &params.append_history {
            events.push(TaskEventRowV2 {
                schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
                event_id: format!("EV-{next_event:04}"),
                at: entry.at,
                by: entry.by.clone(),
                event_type: entry.event.clone(),
                note: entry.note.clone(),
                from_status: entry.from_status,
                to_status: entry.to_status,
            });
            next_event += 1;
        }
        let event_type = params
            .status_event
            .clone()
            .or_else(|| transition.map(|_| "status_changed".to_string()));
        if let Some(event_type) = event_type {
            events.push(TaskEventRowV2 {
                schema_version: TASK_ARTIFACT_SCHEMA_VERSION,
                event_id: format!("EV-{next_event:04}"),
                at: now,
                by: params.actor.clone(),
                event_type,
                note: params.status_note.clone(),
                from_status: transition.map(|(from, _)| from),
                to_status: transition.map(|(_, to)| to),
            });
        }
        for event in &events {
            event.validate()?;
        }

        let mut envelope = bundle.envelope.clone();
        envelope.status = target_status;
        envelope.updated_at = now;
        envelope.validate()?;

        Ok(TaskCommitIntent {
            schema_version: COMMIT_INTENT_SCHEMA_VERSION,
            task_id: params.task_id.clone(),
            events_len: self.events_len(&params.task_id)?,
            events,
            envelope,
        })
    }

    /// Roll a committed decision onto the bundle. Idempotent: the recorded
    /// pre-apply length makes a repeat run produce the same bytes as the
    /// first, whether the first appended nothing, part of a row, or all of
    /// them.
    fn apply_committed(
        &self,
        journal_id: &str,
        intent: &TaskCommitIntent,
    ) -> Result<(), OrbitError> {
        if intent.schema_version != COMMIT_INTENT_SCHEMA_VERSION {
            return Err(OrbitError::Store(format!(
                "task commit journal '{journal_id}' uses unsupported intent schema {}",
                intent.schema_version
            )));
        }
        fail_if_injected(CoordinationFault::DuringApply)?;
        let bundle_dir = self.bundle_store.bundle_path(&intent.task_id)?;
        truncate_jsonl_file(&bundle_dir.join(TASK_EVENTS_FILE_NAME), intent.events_len)?;
        for event in &intent.events {
            self.bundle_store.append_event(&intent.task_id, event)?;
        }
        self.bundle_store
            .rewrite_envelope(&intent.task_id, &intent.envelope)?;
        self.store.finish_task_commit_journal(journal_id)?;
        self.clear_pending_marker()?;
        if let Err(error) = self
            .registry
            .replace_task_index(&self.workspace_id, &intent.envelope)
        {
            orbit_common::tracing::warn!(
                target: "orbit.store.task_commit",
                task_id = %intent.task_id,
                workspace_id = %self.workspace_id,
                error = %error,
                "task commit was published but the generated task index update failed",
            );
        }
        Ok(())
    }

    /// Abandon an undecided commit. Nothing on the bundle changed, so this
    /// only has to retire the journal row and the marker; failing here leaves
    /// both in place and the partition closed until recovery succeeds.
    fn compensate_prepared(&self, journal_id: &str) -> Result<(), OrbitError> {
        fail_if_injected(CoordinationFault::DuringCompensation)?;
        self.store.abort_task_commit_journal(journal_id)?;
        self.clear_pending_marker()
    }

    fn events_len(&self, task_id: &str) -> Result<u64, OrbitError> {
        let path = self
            .bundle_store
            .bundle_path(task_id)?
            .join(TASK_EVENTS_FILE_NAME);
        match std::fs::metadata(&path) {
            Ok(metadata) => Ok(metadata.len()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(error) => Err(OrbitError::from_write_io(&path, error)),
        }
    }

    fn lock_target(&self) -> PathBuf {
        self.partition_dir.join(COORDINATION_LOCK_FILE)
    }

    fn pending_marker_path(&self) -> PathBuf {
        self.partition_dir.join(PENDING_MARKER_FILE)
    }

    fn write_pending_marker(&self, journal_id: &str) -> Result<(), OrbitError> {
        let path = self.pending_marker_path();
        atomic_write_text(&path, journal_id)
            .map_err(|error| OrbitError::from_write_io(&path, error))
    }

    fn clear_pending_marker(&self) -> Result<(), OrbitError> {
        remove_file_if_present(&self.pending_marker_path())
    }

    #[cfg(test)]
    pub(crate) fn pending_marker_exists(&self) -> bool {
        self.pending_marker_path().try_exists().unwrap_or(false)
    }

    #[cfg(test)]
    pub(crate) fn store_handle(&self) -> &Store {
        &self.store
    }
}

fn remove_file_if_present(path: &Path) -> Result<(), OrbitError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(OrbitError::from_write_io(path, error)),
    }
}

/// A journal id that cannot collide across threads, processes, or a clock
/// that reads as zero — the same rule the reservation row ids follow.
fn unique_journal_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let sequence = JOURNAL_ID_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("commit-{nanos}-{}-{sequence}", std::process::id())
}

#[cfg(test)]
mod tests;
