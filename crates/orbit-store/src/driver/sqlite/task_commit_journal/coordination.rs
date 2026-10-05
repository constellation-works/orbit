//! Task coordination row insert, lookup and payload replacement.

use orbit_common::OrbitError;
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
            ).map_err(|e| OrbitError::Store(e.to_string()))?;
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
            ).map(|n| n == 1).map_err(|e| OrbitError::Store(e.to_string()))
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
            .map_err(|error| OrbitError::Store(error.to_string()))
        })
    }
}
