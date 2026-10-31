//! The executor census rows admission orders against [ORB-17304].

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::{Connection, TransactionBehavior, params};

use super::super::codec::stamp;
use crate::Store;
use crate::contracts::{ExecutorCapability, ExecutorCensusEntry};

/// Insert or replace `entry` as its machine's row in `workspace`, unless the
/// stored row was seen later. Runs inside the caller's transaction, so an
/// admission writes it in the same commit as its receipt.
pub(crate) fn upsert_executor_census_in(
    conn: &Connection,
    workspace: &str,
    entry: &ExecutorCensusEntry,
) -> Result<(), OrbitError> {
    let capability = serde_json::to_string(&entry.capability)
        .map_err(|error| OrbitError::Store(error.to_string()))?;
    conn.execute(
        "INSERT INTO executor_census(workspace_id, machine_id, capability_json, run_id, seen_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(workspace_id, machine_id) DO UPDATE SET
             capability_json = excluded.capability_json,
             run_id = excluded.run_id,
             seen_at = excluded.seen_at
         WHERE excluded.seen_at >= executor_census.seen_at",
        params![
            workspace,
            entry.machine_id,
            capability,
            entry.run_id,
            stamp(entry.seen_at),
        ],
    )
    .map_err(sqlite_store_error)?;
    Ok(())
}

impl Store {
    /// Record `entry` in its own transaction: the owner's own row, written
    /// by its local drain.
    pub(crate) fn upsert_executor_census(
        &self,
        workspace: &str,
        entry: &ExecutorCensusEntry,
    ) -> Result<(), OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            upsert_executor_census_in(tx.connection(), workspace, entry)
        })
    }

    /// Every census row of `workspace`, one per machine. A store without the
    /// table reads as an empty census, and a row this binary cannot decode
    /// is left out rather than failing the read.
    pub(crate) fn executor_census_rows(
        &self,
        workspace: &str,
    ) -> Result<Vec<ExecutorCensusEntry>, OrbitError> {
        self.with_read_connection(|conn| {
            let table: bool = conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'executor_census')",
                    [],
                    |row| row.get(0),
                )
                .map_err(sqlite_store_error)?;
            if !table {
                return Ok(Vec::new());
            }
            let mut stmt = conn
                .prepare(
                    "SELECT machine_id, capability_json, run_id, seen_at
                     FROM executor_census WHERE workspace_id = ?1
                     ORDER BY machine_id ASC",
                )
                .map_err(sqlite_store_error)?;
            let rows = stmt
                .query_map(params![workspace], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                })
                .map_err(sqlite_store_error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(sqlite_store_error)?;
            Ok(rows
                .into_iter()
                .filter_map(|(machine_id, capability, run_id, seen_at)| {
                    let capability = serde_json::from_str::<ExecutorCapability>(&capability).ok()?;
                    let seen_at = DateTime::parse_from_rfc3339(&seen_at)
                        .ok()?
                        .with_timezone(&Utc);
                    Some(ExecutorCensusEntry {
                        machine_id,
                        capability,
                        run_id,
                        seen_at,
                    })
                })
                .collect())
        })
    }
}
