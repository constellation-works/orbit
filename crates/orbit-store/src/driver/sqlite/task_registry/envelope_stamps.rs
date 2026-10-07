//! Persisted envelope stamps: the freshness scan's proofs, kept across
//! processes so a cold listing need not parse every `task.yaml` again.
//!
//! The table is a derived cache, never authority. Each row says "the envelope
//! file with this stamp matched the index row with this fingerprint"; a reader
//! trusts it only while the file still reports that stamp and the index row
//! still has that fingerprint, so a stale, missing, or foreign row costs one
//! parse and nothing else. It is created on first record rather than by schema
//! setup, so the registry format (and the v6 recognition in `schema`) is
//! unchanged for every registry that never recorded a stamp, and a read-only
//! registry simply has none.

use std::collections::HashMap;

use orbit_common::OrbitError;
use rusqlite::{Connection, TransactionBehavior, params};

use super::partition_id::validate_partition_id;
use super::store::TaskRegistryStore;
use crate::contracts::EnvelopeStampRecord;

const ENVELOPE_STAMPS_TABLE: &str = "task_envelope_stamps";

/// Create the stamps table. Also applied to the reference schema when the
/// registry being recognized already carries the table.
pub(super) fn ensure_envelope_stamps(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS task_envelope_stamps (
            task_id TEXT PRIMARY KEY,
            workspace_id TEXT NOT NULL,
            stamp TEXT NOT NULL,
            fingerprint TEXT NOT NULL,
            FOREIGN KEY(task_id) REFERENCES task_bundle_bindings(task_id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_task_envelope_stamps_workspace
            ON task_envelope_stamps(workspace_id);",
    )
    .map_err(|e| OrbitError::Store(format!("ensure task envelope stamps: {e}")))
}

pub(super) fn has_envelope_stamps(conn: &Connection) -> Result<bool, OrbitError> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1)",
        [ENVELOPE_STAMPS_TABLE],
        |row| row.get(0),
    )
    .map_err(|e| OrbitError::Store(e.to_string()))
}

impl TaskRegistryStore {
    /// Every stamp recorded for `partition_id`, by task id; empty when none
    /// was ever recorded.
    pub fn envelope_stamps_for_workspace(
        &self,
        partition_id: &str,
    ) -> Result<HashMap<String, EnvelopeStampRecord>, OrbitError> {
        let partition_id = validate_partition_id(partition_id)?;
        let conn = self.read()?;
        if !has_envelope_stamps(&conn)? {
            return Ok(HashMap::new());
        }
        let mut stmt = conn
            .prepare_cached(
                "SELECT task_id, stamp, fingerprint FROM task_envelope_stamps
                 WHERE workspace_id = ?1",
            )
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        let rows = stmt
            .query_map([&partition_id], |row| {
                Ok(EnvelopeStampRecord {
                    task_id: row.get(0)?,
                    stamp: row.get(1)?,
                    fingerprint: row.get(2)?,
                })
            })
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        rows.map(|row| {
            row.map(|record| (record.task_id.clone(), record))
                .map_err(|e| OrbitError::Store(e.to_string()))
        })
        .collect()
    }

    /// Record stamps for tasks registered to `partition_id`, replacing older
    /// ones. A task no longer registered there is skipped.
    pub fn record_envelope_stamps(
        &self,
        partition_id: &str,
        records: &[EnvelopeStampRecord],
    ) -> Result<(), OrbitError> {
        let partition_id = validate_partition_id(partition_id)?;
        if records.is_empty() {
            return Ok(());
        }
        let mut conn = self
            .conn
            .lock()
            .map_err(|e| OrbitError::Store(format!("mutex poisoned: {e}")))?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        ensure_envelope_stamps(&tx)?;
        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO task_envelope_stamps(task_id, workspace_id, stamp, fingerprint)
                     SELECT ?1, ?2, ?3, ?4 WHERE EXISTS (
                         SELECT 1 FROM task_bundle_bindings
                         WHERE task_id = ?1 AND workspace_id = ?2
                     )
                     ON CONFLICT(task_id) DO UPDATE SET
                         workspace_id = excluded.workspace_id,
                         stamp = excluded.stamp,
                         fingerprint = excluded.fingerprint",
                )
                .map_err(|e| OrbitError::Store(e.to_string()))?;
            for record in records {
                stmt.execute(params![
                    record.task_id,
                    partition_id,
                    record.stamp,
                    record.fingerprint
                ])
                .map_err(|e| OrbitError::Store(e.to_string()))?;
            }
        }
        tx.commit().map_err(|e| OrbitError::Store(e.to_string()))
    }
}
