//! The durable commit decision behind the task/reservation commit boundary.
//!
//! Task bundles are files, the task index is one SQLite database, and
//! reservations are another. No single technology can publish all three
//! atomically, so the decision itself is made in one place: a journal row in
//! the reservation database, flipped from `prepared` to `committed` in the
//! *same* transaction that inserts the reservation and the dependent
//! coordination rows. Everything before that transaction is undone on
//! failure; everything after it is replayed until it lands.
//!
//! This module owns only the SQL. The protocol, its file-side apply, and the
//! serialization boundary live in `repository::task::coordination`.

use orbit_common::OrbitError;
use rusqlite::{OptionalExtension, TransactionBehavior, params};

use crate::contracts::{
    TaskCommitJournalRecord, TaskCommitJournalState, TaskCoordinationRow,
    TaskReservationReserveParams, TaskReservationReserveResult,
};
use crate::driver::sqlite::task_reservation_store::reserve_files_in_tx;
use crate::{Store, StoreTx};

/// Outcome of the one transaction that decides a commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum JournalCommitOutcome {
    Committed(Option<TaskReservationReserveResult>),
    /// Reservation files overlap an active reservation; nothing was written.
    Conflicted(TaskReservationReserveResult),
    /// A dependent coordination row already exists under this identity.
    RowExists {
        kind: String,
        row_id: String,
    },
}

impl Store {
    /// Record an undecided commit intent durably, before anything else moves.
    ///
    /// A `prepared` row is the evidence that lets recovery tell "this process
    /// died before deciding" from "this process decided and died before
    /// applying". Its own transaction commits, so the row survives the crash
    /// that the next step might not.
    pub(crate) fn prepare_task_commit_journal(
        &self,
        journal_id: &str,
        workspace_id: &str,
        task_id: &str,
        intent_json: &str,
    ) -> Result<(), OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            tx.tx
                .execute(
                    "INSERT INTO task_commit_journal(
                        journal_id, workspace_id, task_id, state, intent_json,
                        created_at, committed_at, applied_at, reservation_id
                     ) VALUES (?1, ?2, ?3, 'prepared', ?4, ?5, NULL, NULL, NULL)",
                    params![
                        journal_id,
                        workspace_id,
                        task_id,
                        intent_json,
                        crate::now_string()
                    ],
                )
                .map_err(|error| OrbitError::Store(error.to_string()))?;
            Ok(())
        })
    }

    /// The commit point: reservation rows, coordination rows, and the
    /// `prepared → committed` flip in one transaction.
    ///
    /// Any refusal (a reservation conflict, a duplicate coordination row)
    /// rolls the whole transaction back, leaving the journal `prepared` for
    /// the caller to compensate. A returned `Committed` is durable.
    pub(crate) fn commit_task_commit_journal(
        &self,
        journal_id: &str,
        reservation: Option<&TaskReservationReserveParams>,
        rows: &[TaskCoordinationRow],
    ) -> Result<JournalCommitOutcome, OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let workspace_id: String = tx
                .tx
                .query_row(
                    "SELECT workspace_id FROM task_commit_journal
                     WHERE journal_id = ?1 AND state = 'prepared'",
                    params![journal_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|error| OrbitError::Store(error.to_string()))?
                .ok_or_else(|| {
                    OrbitError::Store(format!(
                        "task commit journal '{journal_id}' is not prepared; refusing to commit"
                    ))
                })?;

            if let Some(existing) = first_existing_coordination_row(tx, &workspace_id, rows)? {
                return Ok(JournalCommitOutcome::RowExists {
                    kind: existing.0,
                    row_id: existing.1,
                });
            }

            let reserved = match reservation {
                Some(params) => {
                    let result = reserve_files_in_tx(tx, params)?;
                    if !result.reserved {
                        return Ok(JournalCommitOutcome::Conflicted(result));
                    }
                    Some(result)
                }
                None => None,
            };

            let now = crate::now_string();
            for row in rows {
                tx.tx
                    .execute(
                        "INSERT INTO task_coordination_rows(
                            workspace_id, kind, row_id, payload_json, journal_id, created_at
                         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        params![
                            workspace_id,
                            row.kind,
                            row.row_id,
                            row.payload_json,
                            journal_id,
                            now
                        ],
                    )
                    .map_err(|error| OrbitError::Store(error.to_string()))?;
            }

            let reservation_id = reserved
                .as_ref()
                .and_then(|result| result.reservation_id.clone());
            let affected = tx
                .tx
                .execute(
                    "UPDATE task_commit_journal
                     SET state = 'committed', committed_at = ?2, reservation_id = ?3
                     WHERE journal_id = ?1 AND state = 'prepared'",
                    params![journal_id, now, reservation_id],
                )
                .map_err(|error| OrbitError::Store(error.to_string()))?;
            if affected != 1 {
                return Err(OrbitError::Store(format!(
                    "task commit journal '{journal_id}' changed state during its own commit"
                )));
            }
            Ok(JournalCommitOutcome::Committed(reserved))
        })
    }

    /// Mark a committed decision fully applied to the task bundle.
    pub(crate) fn finish_task_commit_journal(&self, journal_id: &str) -> Result<(), OrbitError> {
        self.settle_task_commit_journal(journal_id, TaskCommitJournalState::Applied, "committed")
    }

    /// Mark an undecided intent abandoned after its pre-commit state was
    /// restored.
    pub(crate) fn abort_task_commit_journal(&self, journal_id: &str) -> Result<(), OrbitError> {
        self.settle_task_commit_journal(journal_id, TaskCommitJournalState::Aborted, "prepared")
    }

    fn settle_task_commit_journal(
        &self,
        journal_id: &str,
        state: TaskCommitJournalState,
        from_state: &str,
    ) -> Result<(), OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let affected = tx
                .tx
                .execute(
                    "UPDATE task_commit_journal
                     SET state = ?2,
                         applied_at = CASE WHEN ?2 = 'applied' THEN ?3 ELSE applied_at END
                     WHERE journal_id = ?1 AND state = ?4",
                    params![journal_id, state.as_str(), crate::now_string(), from_state],
                )
                .map_err(|error| OrbitError::Store(error.to_string()))?;
            if affected != 1 {
                return Err(OrbitError::Store(format!(
                    "task commit journal '{journal_id}' is no longer '{from_state}'; \
                     refusing to record '{}'",
                    state.as_str()
                )));
            }
            Ok(())
        })
    }

    /// Every unsettled decision for a workspace, oldest first. Recovery
    /// replays exactly this list.
    pub(crate) fn unsettled_task_commit_journal(
        &self,
        workspace_id: &str,
    ) -> Result<Vec<TaskCommitJournalRecord>, OrbitError> {
        self.with_read_connection(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT journal_id, workspace_id, task_id, state, intent_json, reservation_id
                     FROM task_commit_journal
                     WHERE workspace_id = ?1 AND state IN ('prepared', 'committed')
                     ORDER BY created_at ASC, journal_id ASC",
                )
                .map_err(|error| OrbitError::Store(error.to_string()))?;
            let rows = stmt
                .query_map(params![workspace_id], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, Option<String>>(5)?,
                    ))
                })
                .map_err(|error| OrbitError::Store(error.to_string()))?;

            let mut records = Vec::new();
            for row in rows {
                let (journal_id, workspace_id, task_id, state, intent_json, reservation_id) =
                    row.map_err(|error| OrbitError::Store(error.to_string()))?;
                let state = TaskCommitJournalState::parse(&state).ok_or_else(|| {
                    OrbitError::Store(format!(
                        "task commit journal '{journal_id}' has unknown state '{state}'"
                    ))
                })?;
                records.push(TaskCommitJournalRecord {
                    journal_id,
                    workspace_id,
                    task_id,
                    state,
                    intent_json,
                    reservation_id,
                });
            }
            Ok(records)
        })
    }

    /// Read one journal row's state. Test-only: production callers read the
    /// unsettled set, never a single row's label.
    #[cfg(test)]
    pub(crate) fn task_commit_journal_state(
        &self,
        journal_id: &str,
    ) -> Result<Option<TaskCommitJournalState>, OrbitError> {
        self.with_read_connection(|conn| {
            let state: Option<String> = conn
                .query_row(
                    "SELECT state FROM task_commit_journal WHERE journal_id = ?1",
                    params![journal_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|error| OrbitError::Store(error.to_string()))?;
            state
                .map(|state| {
                    TaskCommitJournalState::parse(&state).ok_or_else(|| {
                        OrbitError::Store(format!(
                            "task commit journal '{journal_id}' has unknown state '{state}'"
                        ))
                    })
                })
                .transpose()
        })
    }

    /// Dependent coordination rows published for a workspace, by kind.
    pub(crate) fn task_coordination_rows(
        &self,
        workspace_id: &str,
        kind: &str,
    ) -> Result<Vec<TaskCoordinationRow>, OrbitError> {
        self.with_read_connection(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT kind, row_id, payload_json
                     FROM task_coordination_rows
                     WHERE workspace_id = ?1 AND kind = ?2
                     ORDER BY row_id ASC",
                )
                .map_err(|error| OrbitError::Store(error.to_string()))?;
            let rows = stmt
                .query_map(params![workspace_id, kind], |row| {
                    Ok(TaskCoordinationRow {
                        kind: row.get(0)?,
                        row_id: row.get(1)?,
                        payload_json: row.get(2)?,
                    })
                })
                .map_err(|error| OrbitError::Store(error.to_string()))?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|error| OrbitError::Store(error.to_string()))
        })
    }
}

/// The first requested row identity that is already published, if any.
fn first_existing_coordination_row(
    tx: &mut StoreTx<'_>,
    workspace_id: &str,
    rows: &[TaskCoordinationRow],
) -> Result<Option<(String, String)>, OrbitError> {
    for row in rows {
        let existing: Option<String> = tx
            .tx
            .query_row(
                "SELECT row_id FROM task_coordination_rows
                 WHERE workspace_id = ?1 AND kind = ?2 AND row_id = ?3",
                params![workspace_id, row.kind, row.row_id],
                |found| found.get(0),
            )
            .optional()
            .map_err(|error| OrbitError::Store(error.to_string()))?;
        if existing.is_some() {
            return Ok(Some((row.kind.clone(), row.row_id.clone())));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests;
