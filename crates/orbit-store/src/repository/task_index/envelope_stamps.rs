//! Persisted envelope stamps: the freshness scan's proofs, kept across
//! processes so a cold listing need not parse every `task.yaml` again.
//!
//! The table is a derived cache, never authority. Each row says "the envelope
//! file with this stamp matched the index row with this fingerprint"; a reader
//! trusts it only while the file still reports that stamp and the index row
//! still has that fingerprint, so a stale, missing, or foreign row costs one
//! parse and nothing else. The registry baseline (ledger v1) creates the
//! table. Only a pre-ledger registry opened read-only may lack it, because
//! binaries before the ledger created it on first record; it then has no
//! stamps.

use std::collections::HashMap;

use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::{Connection, ErrorCode, TransactionBehavior, params};

use super::partition_id::validate_partition_id;
use super::store::TaskRegistryStore;
use crate::contract::EnvelopeStampRecord;

const ENVELOPE_STAMPS_TABLE: &str = "task_envelope_stamps";

impl TaskRegistryStore {
    /// Every stamp recorded for `partition_id`, by task id; empty when none
    /// was ever recorded.
    pub fn envelope_stamps_for_workspace(
        &self,
        partition_id: &str,
    ) -> Result<HashMap<String, EnvelopeStampRecord>, OrbitError> {
        let partition_id = validate_partition_id(partition_id)?;
        let conn = self.read()?;
        if self.db.pre_ledger() && !has_envelope_stamps(&conn)? {
            return Ok(HashMap::new());
        }
        let mut stmt = conn
            .prepare_cached(
                "SELECT task_id, stamp, fingerprint FROM task_envelope_stamps
                 WHERE workspace_id = ?1",
            )
            .map_err(sqlite_store_error)?;
        let rows = stmt
            .query_map([&partition_id], |row| {
                Ok(EnvelopeStampRecord {
                    task_id: row.get(0)?,
                    stamp: row.get(1)?,
                    fingerprint: row.get(2)?,
                })
            })
            .map_err(sqlite_store_error)?;
        rows.map(|row| {
            row.map(|record| (record.task_id.clone(), record))
                .map_err(sqlite_store_error)
        })
        .collect()
    }

    /// Record stamps for tasks registered to `partition_id`, replacing older
    /// ones. A task no longer registered there is skipped.
    ///
    /// Opportunistic, because a freshness scan records them from inside a
    /// task read's boundary section and they are only a cache: it never waits.
    /// When another thread holds this process's writer connection, or another
    /// connection holds the registry's write lock, it records nothing and
    /// returns `Ok(false)`, leaving the next scan to parse those envelopes and
    /// try again. It also skips the per-commit fsync registry writes otherwise
    /// pay; a stamp lost to a power cut costs one parse. Waiting out the 5 s
    /// busy timeout behind drain writers, and the fsync after it, held
    /// ordinary task reads in the boundary for seconds (ORB-15534).
    pub fn record_envelope_stamps(
        &self,
        partition_id: &str,
        records: &[EnvelopeStampRecord],
    ) -> Result<bool, OrbitError> {
        let partition_id = validate_partition_id(partition_id)?;
        if records.is_empty() {
            return Ok(true);
        }
        self.db
            .cache_write(|conn| write_envelope_stamps(conn, &partition_id, records))
    }
}

fn has_envelope_stamps(conn: &Connection) -> Result<bool, OrbitError> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1)",
        [ENVELOPE_STAMPS_TABLE],
        |row| row.get(0),
    )
    .map_err(sqlite_store_error)
}

/// Upsert `records` in one transaction. `Ok(false)` when another connection
/// holds the write lock, which the no-wait busy timeout reports at once.
fn write_envelope_stamps(
    conn: &mut Connection,
    partition_id: &str,
    records: &[EnvelopeStampRecord],
) -> Result<bool, OrbitError> {
    let tx = match conn.transaction_with_behavior(TransactionBehavior::Immediate) {
        Ok(tx) => tx,
        Err(error) if error.sqlite_error_code() == Some(ErrorCode::DatabaseBusy) => {
            return Ok(false);
        }
        Err(error) => return Err(sqlite_store_error(error)),
    };
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
            .map_err(sqlite_store_error)?;
        for record in records {
            stmt.execute(params![
                record.task_id,
                partition_id,
                record.stamp,
                record.fingerprint
            ])
            .map_err(sqlite_store_error)?;
        }
    }
    tx.commit().map_err(sqlite_store_error)?;
    Ok(true)
}
