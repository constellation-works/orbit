//! Per-routine baseline and slot cursor.

use orbit_common::OrbitError;
use rusqlite::{OptionalExtension, TransactionBehavior, params};

use super::RoutineCursor;
use crate::Store;

impl Store {
    /// Cursor for one routine, if this host has observed it before.
    pub fn routine_cursor(&self, routine_name: &str) -> Result<Option<RoutineCursor>, OrbitError> {
        let conn = self.read()?;
        conn.query_row(
            "SELECT routine_name, baseline_at, last_slot FROM routine_cursors
             WHERE routine_name = ?1",
            params![routine_name],
            |row| {
                Ok(RoutineCursor {
                    routine_name: row.get(0)?,
                    baseline_at: row.get(1)?,
                    last_slot: row.get(2)?,
                })
            },
        )
        .optional()
        .map_err(|error| OrbitError::Store(error.to_string()))
    }

    /// Record the first observation of a routine on this host. Idempotent:
    /// an existing cursor is left untouched, so the baseline never moves
    /// backwards or forwards once set. Returns whether a row was created.
    pub fn routine_record_baseline(
        &self,
        routine_name: &str,
        baseline_at: &str,
    ) -> Result<bool, OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let now = crate::now_string();
            let inserted = tx
                .tx
                .execute(
                    "INSERT OR IGNORE INTO routine_cursors
                     (routine_name, baseline_at, last_slot, updated_at)
                     VALUES (?1, ?2, NULL, ?3)",
                    params![routine_name, baseline_at, now],
                )
                .map_err(|error| OrbitError::Store(error.to_string()))?;
            Ok(inserted > 0)
        })
    }
}
