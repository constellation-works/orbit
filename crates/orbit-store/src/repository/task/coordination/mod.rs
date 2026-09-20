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
//! Every runtime composition participates in the same filesystem locks. A
//! durable required marker prevents legacy task stores from accessing a
//! partition after coordinated composition has activated it. A shared host
//! lock additionally protects dependency reads across workspace partitions;
//! admission takes it exclusively before its partition lock.
//!
//! # Recovery before exposure
//!
//! The pending marker is the cheap signal that a commit is in flight or was
//! interrupted. Any participant that sees it takes the boundary exclusively
//! and replays the journal before it reads or writes anything, so no caller
//! observes a committed reservation whose task transition has not landed. A
//! compensation or replay that fails leaves the marker in place and returns
//! the error: the partition stays closed until recovery succeeds.
//!
//! # Who takes the boundary
//!
//! Coordinated task mutations enter through `TaskV2Store::in_boundary` or
//! `TaskV2Store::with_task_lock` (boundary first, then the bundle lock).
//! Coordinated reservation mutations enter through
//! `SqliteTaskReservationStoreBackend::in_boundary`, including methods that
//! look like getters but lazily mark expired rows released
//! (`list_active_task_reservations`, `show_workspace_claim`). The one
//! deliberate exception is `inspect_active_task_reservations`: it is a true
//! read (it does not expire rows) so `orbit doctor` stays strictly
//! read-only, and it therefore stays outside the boundary.

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
const REQUIRED_MARKER_FILE: &str = ".task-commit-required";
const COMMIT_INTENT_SCHEMA_VERSION: u32 = 2;

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
    /// After evidence documents/comments, before artifact and envelope publication.
    DuringEvidenceApply,
    /// While abandoning an undecided commit.
    DuringCompensation,
    /// While replaying the journal on the next entry.
    DuringRecovery,
}

#[cfg(test)]
thread_local! {
    static BEFORE_ORDINARY_LOCK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
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
    static BOUNDARY_DEPTH: std::cell::RefCell<Vec<PathBuf>> = const { std::cell::RefCell::new(Vec::new()) };
}

struct BoundaryDepth;

impl BoundaryDepth {
    fn enter(partition: &Path) -> Self {
        BOUNDARY_DEPTH.with(|depth| depth.borrow_mut().push(partition.to_path_buf()));
        Self
    }

    fn active(partition: &Path) -> bool {
        BOUNDARY_DEPTH.with(|depth| depth.borrow().iter().any(|held| held == partition))
    }
}

impl Drop for BoundaryDepth {
    fn drop(&mut self) {
        BOUNDARY_DEPTH.with(|depth| {
            depth.borrow_mut().pop();
        });
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
    #[serde(default)]
    evidence: lifecycle::EvidenceIntent,
}

/// One task-store partition's durable commit and recovery authority.
///
/// Compositions for the same partition share filesystem locks and the durable
/// journal. Legacy compositions refuse activated partitions. See the module docs for
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
        let boundary = Self {
            bundle_store: TaskBundleStoreV2::new(registry.clone(), workspace_id.clone()),
            store,
            registry,
            workspace_id,
            partition_dir,
        };
        with_exclusive_file_lock(
            &boundary.lock_target(),
            COORDINATION_LOCK_LABEL,
            || -> Result<(), OrbitError> {
                let marker = boundary.partition_dir.join(REQUIRED_MARKER_FILE);
                if marker.try_exists()? {
                    boundary.verify_journal_binding()?;
                } else {
                    let path = serde_json::to_string(&boundary.store.task_commit_database_path()?)
                        .map_err(|error| OrbitError::Store(error.to_string()))?;
                    atomic_write_text(&marker, &path)
                        .map_err(|error| OrbitError::from_write_io(&marker, error))?;
                }
                Ok(())
            },
        )?;
        Ok(boundary)
    }

    /// Observation-only handle: no directory creation, lock files, or marker writes.
    pub fn for_observation(
        store: Store,
        registry: TaskRegistryStore,
        workspace_id: String,
    ) -> Result<Self, OrbitError> {
        let partition_dir = registry.workspace_partition_dir(&workspace_id)?;
        Ok(Self {
            bundle_store: TaskBundleStoreV2::new(registry.clone(), workspace_id.clone()),
            store,
            registry,
            workspace_id,
            partition_dir,
        })
    }

    fn verify_journal_binding(&self) -> Result<(), OrbitError> {
        let marker = self.partition_dir.join(REQUIRED_MARKER_FILE);
        if marker.try_exists()? {
            let path: PathBuf = serde_json::from_str(&std::fs::read_to_string(marker)?)
                .map_err(|error| OrbitError::Store(error.to_string()))?;
            if path != self.store.task_commit_database_path()? {
                return Err(OrbitError::Store(
                    "task partition is bound to a different coordination journal".into(),
                ));
            }
        }
        Ok(())
    }

    /// Legacy compositions may serve an uncoordinated partition, but cannot
    /// race or overwrite one that has opted into durable coordination.
    pub(crate) fn enter_uncoordinated<T>(
        registry: &TaskRegistryStore,
        workspace_id: &str,
        op: impl FnOnce() -> Result<T, OrbitError>,
    ) -> Result<T, OrbitError> {
        let partition = registry.workspace_partition_dir(workspace_id)?;
        let host_lock = host_lock_for_partition(&partition);
        with_shared_file_lock(&host_lock, COORDINATION_LOCK_LABEL, || {
            with_shared_file_lock(
                &partition.join(COORDINATION_LOCK_FILE),
                COORDINATION_LOCK_LABEL,
                || {
                    if partition.join(REQUIRED_MARKER_FILE).try_exists()? {
                        return Err(OrbitError::Store(
                            "this task partition requires coordinated backends".into(),
                        ));
                    }
                    op()
                },
            )
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
        with_shared_file_lock(&self.host_lock_target(), COORDINATION_LOCK_LABEL, || {
            self.enter_ordinary_locked(op)
        })
    }

    fn enter_ordinary_locked<T>(
        &self,
        op: impl FnOnce() -> Result<T, OrbitError>,
    ) -> Result<T, OrbitError> {
        let mut op = Some(op);
        loop {
            self.recover_if_pending()?;
            #[cfg(test)]
            BEFORE_ORDINARY_LOCK.with(|hook| {
                if let Some(hook) = hook.borrow_mut().take() {
                    hook();
                }
            });
            let result =
                with_shared_file_lock(&self.lock_target(), COORDINATION_LOCK_LABEL, || {
                    // A commit may have crashed while we waited for this lock.
                    // Drop the shared acquisition before taking recovery exclusive.
                    if !BoundaryDepth::active(&self.partition_dir)
                        && self.pending_marker_path().try_exists()?
                    {
                        return Ok(None);
                    }
                    let _depth = BoundaryDepth::enter(&self.partition_dir);
                    let operation = op.take().ok_or_else(|| {
                        OrbitError::Store("ordinary boundary operation was already consumed".into())
                    })?;
                    operation().map(Some)
                })?;
            if let Some(result) = result {
                return Ok(result);
            }
        }
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
        with_exclusive_file_lock(&self.host_lock_target(), COORDINATION_LOCK_LABEL, || {
            with_exclusive_file_lock(&self.lock_target(), COORDINATION_LOCK_LABEL, || {
                self.recover_if_pending()?;
                let _depth = BoundaryDepth::enter(&self.partition_dir);
                op()
            })
        })
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
        self.with_admission(|| {
            self.refuse_unscoped_claim_write(&params.task_id)?;
            self.commit_locked(params)
        })
    }

    /// Dependent coordination rows published for this partition, by kind.
    ///
    /// Settles an interrupted commit first, so a caller reading back its own
    /// receipts cannot miss one that a crashed commit had already decided.
    pub fn coordination_rows(
        &self,
        kind: &str,
    ) -> Result<Vec<crate::contracts::TaskCoordinationRow>, OrbitError> {
        self.enter_ordinary(|| self.store.task_coordination_rows(&self.workspace_id, kind))
    }

    /// Replay the journal when a marker says a commit may be unfinished.
    ///
    /// Cheap on the common path: one existence check.
    pub fn recover_if_pending(&self) -> Result<(), OrbitError> {
        if BoundaryDepth::active(&self.partition_dir) {
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
        with_shared_file_lock(&self.host_lock_target(), COORDINATION_LOCK_LABEL, || {
            self.recover_locked()
        })
    }

    fn recover_locked(&self) -> Result<(), OrbitError> {
        with_exclusive_file_lock(&self.lock_target(), COORDINATION_LOCK_LABEL, || {
            let _depth = BoundaryDepth::enter(&self.partition_dir);
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
        self.commit_locked_with_rows(params, &mut |_| Ok(params.rows.clone()))
    }

    fn commit_locked_with_rows(
        &self,
        params: &TaskCoordinationCommitParams,
        make_rows: &mut impl FnMut(
            Option<&crate::contracts::TaskReservationReserveResult>,
        )
            -> Result<Vec<crate::contracts::TaskCoordinationRow>, OrbitError>,
    ) -> Result<TaskCoordinationCommitOutcome, OrbitError> {
        self.commit_locked_effects(
            params,
            make_rows,
            &Default::default(),
            &Default::default(),
            None,
        )
    }

    fn commit_locked_effects(
        &self,
        params: &TaskCoordinationCommitParams,
        make_rows: &mut impl FnMut(
            Option<&crate::contracts::TaskReservationReserveResult>,
        )
            -> Result<Vec<crate::contracts::TaskCoordinationRow>, OrbitError>,
        effects: &crate::contracts::ClaimCommitEffects,
        evidence: &crate::contracts::ClaimEvidence,
        binding: Option<&crate::contracts::ClaimRun>,
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

                let mut intent = self.build_intent(&bundle, params)?;
                self.prepare_claim_evidence(
                    &mut intent,
                    &bundle,
                    evidence,
                    binding,
                    &params.actor,
                    effects,
                )?;
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

                let decided = match self.store.commit_task_commit_journal_effects(
                    &journal_id,
                    params.reservation.as_ref(),
                    &params.rows,
                    make_rows,
                    effects,
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
            evidence: Default::default(),
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
        if !(1..=COMMIT_INTENT_SCHEMA_VERSION).contains(&intent.schema_version) {
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
        self.apply_claim_evidence(intent)?;
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

    fn host_lock_target(&self) -> PathBuf {
        host_lock_for_partition(&self.partition_dir)
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

fn host_lock_for_partition(partition: &Path) -> PathBuf {
    // Partitions are tasks/workspaces/<id>. Keep host metadata beside the
    // registry database, outside the directory enumerated as workspaces.
    partition
        .ancestors()
        .nth(2)
        .unwrap_or(partition)
        .join("host-task-commit")
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

mod admission;
mod handoff;
mod landing;
mod lifecycle;

pub use admission::admission_refusal;

#[cfg(test)]
mod tests;
