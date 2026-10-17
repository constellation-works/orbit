//! Task coordination row insert, lookup, payload replacement and settled
//! claim-receipt deletion.

use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::{OptionalExtension, TransactionBehavior, params};

use crate::Store;
use crate::contracts::TaskCoordinationRow;

impl Store {
    /// Pure-SQL coordination facts (idle receipts) need no bundle replay.
    pub(crate) fn insert_task_coordination_row(
        &self,
        workspace: &str,
        row: &TaskCoordinationRow,
    ) -> Result<(), OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            tx.tx.execute(
                "INSERT INTO task_coordination_rows(workspace_id,kind,row_id,payload_json,journal_id,created_at) VALUES (?1,?2,?3,?4,'sql-only',?5)",
                params![workspace, row.kind, row.row_id, row.payload_json, crate::now_string()],
            ).map_err(sqlite_store_error)?;
            Ok(())
        })
    }

    /// Publish pure-SQL coordination rows in one transaction, each inserted
    /// or, when its identity already exists, given the new payload.
    pub(crate) fn upsert_sql_task_coordination_rows(
        &self,
        workspace: &str,
        rows: &[TaskCoordinationRow],
    ) -> Result<(), OrbitError> {
        if rows.is_empty() {
            return Ok(());
        }
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let now = crate::now_string();
            for row in rows {
                tx.tx.execute(
                    "INSERT INTO task_coordination_rows(workspace_id,kind,row_id,payload_json,journal_id,created_at) VALUES (?1,?2,?3,?4,'sql-only',?5)
                     ON CONFLICT(workspace_id,kind,row_id) DO UPDATE SET payload_json=excluded.payload_json",
                    params![workspace, row.kind, row.row_id, row.payload_json, now],
                ).map_err(sqlite_store_error)?;
            }
            Ok(())
        })
    }

    /// Replace a full receipt by a permanent tombstone under the repository
    /// boundary. A stale compactor cannot overwrite a different generation.
    pub(crate) fn replace_task_coordination_payload(
        &self,
        workspace: &str,
        old: &TaskCoordinationRow,
        payload: &str,
    ) -> Result<bool, OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            tx.tx.execute(
                "UPDATE task_coordination_rows SET payload_json=?5 WHERE workspace_id=?1 AND kind=?2 AND row_id=?3 AND payload_json=?4",
                params![workspace, old.kind, old.row_id, old.payload_json, payload],
            ).map(|n| n == 1).map_err(sqlite_store_error)
        })
    }

    /// Delete the claim-mutation receipts of `claim_ids` written before
    /// `created_before`, with the `sidecar_kind` rows keyed by the same
    /// receipt ids, in one transaction. A receipt's `input` is the encoded
    /// invocation tuple whose second element is its claim id. A receipt whose
    /// commit the journal has not finished applying is kept for recovery.
    /// Returns how many receipts went.
    pub(crate) fn delete_claim_mutation_receipts(
        &self,
        workspace: &str,
        receipt_kind: &str,
        sidecar_kind: &str,
        claim_ids: &[String],
        created_before: &str,
    ) -> Result<usize, OrbitError> {
        let claim_ids =
            serde_json::to_string(claim_ids).map_err(|e| OrbitError::Store(e.to_string()))?;
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let receipt_ids = {
                let mut stmt = tx
                    .tx
                    .prepare(
                        "SELECT r.row_id FROM task_coordination_rows r
                         WHERE r.workspace_id = ?1 AND r.kind = ?2 AND r.created_at < ?3
                           AND json_extract(json_extract(r.payload_json, '$.input'), '$[1]')
                               IN (SELECT value FROM json_each(?4))
                           AND NOT EXISTS (
                               SELECT 1 FROM task_commit_journal j
                               WHERE j.journal_id = r.journal_id
                                 AND j.state IN ('prepared', 'committed'))",
                    )
                    .map_err(sqlite_store_error)?;
                stmt.query_map(
                    params![workspace, receipt_kind, created_before, claim_ids],
                    |row| row.get::<_, String>(0),
                )
                .map_err(sqlite_store_error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(sqlite_store_error)?
            };
            let mut delete = tx
                .tx
                .prepare(
                    "DELETE FROM task_coordination_rows
                     WHERE workspace_id = ?1 AND kind IN (?2, ?3) AND row_id = ?4",
                )
                .map_err(sqlite_store_error)?;
            for receipt_id in &receipt_ids {
                delete
                    .execute(params![workspace, receipt_kind, sidecar_kind, receipt_id])
                    .map_err(sqlite_store_error)?;
            }
            Ok(receipt_ids.len())
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
                .map_err(sqlite_store_error)?;
            let rows = stmt
                .query_map(params![workspace_id, kind], |row| {
                    Ok(TaskCoordinationRow {
                        kind: row.get(0)?,
                        row_id: row.get(1)?,
                        payload_json: row.get(2)?,
                    })
                })
                .map_err(sqlite_store_error)?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(sqlite_store_error)
        })
    }

    /// Rows of `kind` with the optional payload of `related_kind` at the
    /// same identity. The left join reads the whole set in one statement and
    /// probes the related rows through the coordination primary key.
    pub(crate) fn task_coordination_rows_with_related_payload(
        &self,
        workspace_id: &str,
        kind: &str,
        related_kind: &str,
    ) -> Result<Vec<(TaskCoordinationRow, Option<String>)>, OrbitError> {
        self.with_read_connection(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT r.kind, r.row_id, r.payload_json, related.payload_json
                     FROM task_coordination_rows r
                     LEFT JOIN task_coordination_rows related
                       ON related.workspace_id = r.workspace_id
                         AND related.kind = ?3 AND related.row_id = r.row_id
                     WHERE r.workspace_id = ?1 AND r.kind = ?2
                     ORDER BY r.row_id ASC",
                )
                .map_err(sqlite_store_error)?;
            let rows = stmt
                .query_map(params![workspace_id, kind, related_kind], |row| {
                    Ok((
                        TaskCoordinationRow {
                            kind: row.get(0)?,
                            row_id: row.get(1)?,
                            payload_json: row.get(2)?,
                        },
                        row.get(3)?,
                    ))
                })
                .map_err(sqlite_store_error)?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(sqlite_store_error)
        })
    }

    /// Coordination rows of one kind whose id starts with `prefix`, by an
    /// index-bounded range over the `(workspace_id, kind, row_id)` key.
    /// `prefix` must end in `:` so the exclusive upper bound is its `;`.
    pub(crate) fn task_coordination_rows_with_prefix(
        &self,
        workspace_id: &str,
        kind: &str,
        prefix: &str,
    ) -> Result<Vec<TaskCoordinationRow>, OrbitError> {
        let Some(stem) = prefix.strip_suffix(':') else {
            return Err(OrbitError::InvalidInput(format!(
                "coordination row prefix `{prefix}` must end with ':'"
            )));
        };
        let upper = format!("{stem};");
        self.with_read_connection(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT kind, row_id, payload_json
                     FROM task_coordination_rows
                     WHERE workspace_id = ?1 AND kind = ?2
                       AND row_id >= ?3 AND row_id < ?4
                     ORDER BY row_id ASC",
                )
                .map_err(sqlite_store_error)?;
            let rows = stmt
                .query_map(params![workspace_id, kind, prefix, upper], |row| {
                    Ok(TaskCoordinationRow {
                        kind: row.get(0)?,
                        row_id: row.get(1)?,
                        payload_json: row.get(2)?,
                    })
                })
                .map_err(sqlite_store_error)?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(sqlite_store_error)
        })
    }

    /// Coordination rows of one kind whose payload contains `needle`, a
    /// prefilter for a caller that decodes each and keeps exact matches.
    /// Encode `needle` as the payload encodes the value it stands for, so no
    /// matching row is left out.
    pub(crate) fn task_coordination_rows_containing(
        &self,
        workspace_id: &str,
        kind: &str,
        needle: &str,
    ) -> Result<Vec<TaskCoordinationRow>, OrbitError> {
        self.with_read_connection(|conn| {
            let mut stmt = conn
                .prepare(
                    "SELECT kind, row_id, payload_json
                     FROM task_coordination_rows
                     WHERE workspace_id = ?1 AND kind = ?2 AND instr(payload_json, ?3) > 0
                     ORDER BY row_id ASC",
                )
                .map_err(sqlite_store_error)?;
            let rows = stmt
                .query_map(params![workspace_id, kind, needle], |row| {
                    Ok(TaskCoordinationRow {
                        kind: row.get(0)?,
                        row_id: row.get(1)?,
                        payload_json: row.get(2)?,
                    })
                })
                .map_err(sqlite_store_error)?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(sqlite_store_error)
        })
    }

    /// One published coordination row, looked up by identity.
    pub(crate) fn task_coordination_row(
        &self,
        workspace_id: &str,
        kind: &str,
        row_id: &str,
    ) -> Result<Option<TaskCoordinationRow>, OrbitError> {
        self.with_read_connection(|conn| {
            conn.query_row(
                "SELECT kind, row_id, payload_json
                 FROM task_coordination_rows
                 WHERE workspace_id = ?1 AND kind = ?2 AND row_id = ?3",
                params![workspace_id, kind, row_id],
                |row| {
                    Ok(TaskCoordinationRow {
                        kind: row.get(0)?,
                        row_id: row.get(1)?,
                        payload_json: row.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(sqlite_store_error)
        })
    }
}
