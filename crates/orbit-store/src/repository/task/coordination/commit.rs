//! The commit protocol itself: prepare, decide, apply, compensation and
//! journal replay on recovery.

use super::{
    BoundaryDepth, COMMIT_INTENT_SCHEMA_VERSION, COORDINATION_LOCK_LABEL, CoordinationFault,
    PENDING_MARKER_FILE, TaskCommitBoundary, TaskCommitIntent, fail_if_injected,
};
use crate::contracts::{
    TaskCommitJournalState, TaskCoordinationCommit, TaskCoordinationCommitOutcome,
    TaskCoordinationCommitParams,
};
use crate::driver::file::task_bundle::truncate_jsonl_file;
use crate::driver::sqlite::task_commit_journal::JournalCommitOutcome;
use crate::repository::task::v2::sequencing::next_sequence;
use crate::repository::task::v2_bundle::TaskBundleV2;
use chrono::Utc;
use orbit_common::OrbitError;
use orbit_common::fs::io::{atomic_write_text, with_exclusive_file_lock, with_shared_file_lock};
use orbit_types::task::{TASK_ARTIFACT_SCHEMA_VERSION, TASK_EVENTS_FILE_NAME, TaskEventRowV2};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static JOURNAL_ID_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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

impl TaskCommitBoundary {
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

    pub(super) fn commit_locked(
        &self,
        params: &TaskCoordinationCommitParams,
    ) -> Result<TaskCoordinationCommitOutcome, OrbitError> {
        self.commit_locked_with_rows(params, &mut |_| Ok(params.rows.clone()))
    }

    pub(super) fn commit_locked_with_rows(
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

    pub(super) fn commit_locked_effects(
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

    pub(super) fn pending_marker_path(&self) -> PathBuf {
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
}
