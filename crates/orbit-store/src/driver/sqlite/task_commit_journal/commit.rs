//! The one transaction that decides a task commit.

use orbit_common::OrbitError;
use rusqlite::{OptionalExtension, TransactionBehavior, params};

use super::JournalCommitOutcome;
use crate::contracts::{
    TaskCoordinationRow, TaskReservationReserveParams, TaskReservationReserveResult,
};
use crate::driver::sqlite::task_reservation_store::reserve_files_in_tx;
use crate::{Store, StoreTx};

impl Store {
    /// The commit point: reservation rows, coordination rows, and the
    /// `prepared → committed` flip in one transaction.
    ///
    /// Any refusal (a reservation conflict, a duplicate coordination row)
    /// rolls the whole transaction back, leaving the journal `prepared` for
    /// the caller to compensate. A returned `Committed` is durable.
    pub(crate) fn commit_task_commit_journal_effects(
        &self,
        journal_id: &str,
        reservation: Option<&TaskReservationReserveParams>,
        identities: &[TaskCoordinationRow],
        make_rows: &mut impl FnMut(
            Option<&TaskReservationReserveResult>,
        ) -> Result<Vec<TaskCoordinationRow>, OrbitError>,
        effects: &crate::contracts::ClaimCommitEffects,
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

            if let Some(existing) = first_existing_coordination_row(tx, &workspace_id, identities)?
            {
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

            let rows = make_rows(reserved.as_ref())?;
            if rows.len() != identities.len()
                || rows.iter().zip(identities).any(|(row, identity)| {
                    row.kind != identity.kind || row.row_id != identity.row_id
                })
            {
                return Err(OrbitError::Store(
                    "coordination row identities changed during commit".into(),
                ));
            }
            let now = crate::now_string();
            for row in &rows {
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

            for (old, new) in &effects.replacements {
                if old.kind != new.kind || old.row_id != new.row_id {
                    return Err(OrbitError::Store("claim row identity changed".into()));
                }
                let count = tx.tx.execute(
                    "UPDATE task_coordination_rows SET payload_json=?5, journal_id=?6 WHERE workspace_id=?1 AND kind=?2 AND row_id=?3 AND payload_json=?4",
                    params![workspace_id, old.kind, old.row_id, old.payload_json, new.payload_json, journal_id],
                ).map_err(|e| OrbitError::Store(e.to_string()))?;
                if count != 1 {
                    return Err(OrbitError::InvalidInput("stale_claim".into()));
                }
            }
            if let Some(reservation_id) = &effects.release_reservation {
                tx.tx.execute(
                    "UPDATE task_reservations SET released_at=?3, release_reason='explicit' WHERE reservation_id=?1 AND workspace_id=?2 AND released_at IS NULL",
                    params![reservation_id, workspace_id, now],
                ).map_err(|e| OrbitError::Store(e.to_string()))?;
            }

            if let Some((friction, receipt_id)) = &effects.friction {
                let record = crate::driver::sqlite::friction_write::add_in_transaction(
                    tx.connection(), &workspace_id, friction,
                )?;
                let payload = serde_json::to_string(&record)
                    .map_err(|error| OrbitError::Store(error.to_string()))?;
                tx.tx.execute(
                    "INSERT INTO task_coordination_rows(workspace_id, kind, row_id, payload_json, journal_id, created_at) VALUES (?1, 'distributed-claim-friction-v1', ?2, ?3, ?4, ?5)",
                    params![workspace_id, receipt_id, payload, journal_id, now],
                ).map_err(|error| OrbitError::Store(error.to_string()))?;
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
