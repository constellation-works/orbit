//! Per-routine baseline and slot cursor.

use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
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
        .map_err(sqlite_store_error)
    }

    /// Cursors for every named routine in one query, keyed by routine name.
    pub fn routine_cursors_for(
        &self,
        names: &[String],
    ) -> Result<std::collections::BTreeMap<String, RoutineCursor>, OrbitError> {
        if names.is_empty() {
            return Ok(std::collections::BTreeMap::new());
        }
        let placeholders = vec!["?"; names.len()].join(",");
        let conn = self.read()?;
        let mut stmt = conn
            .prepare(&format!(
                "SELECT routine_name, baseline_at, last_slot FROM routine_cursors
                 WHERE routine_name IN ({placeholders})"
            ))
            .map_err(sqlite_store_error)?;
        let params: Vec<&dyn rusqlite::types::ToSql> = names
            .iter()
            .map(|name| name as &dyn rusqlite::types::ToSql)
            .collect();
        let rows = stmt
            .query_map(params.as_slice(), |row| {
                Ok(RoutineCursor {
                    routine_name: row.get(0)?,
                    baseline_at: row.get(1)?,
                    last_slot: row.get(2)?,
                })
            })
            .map_err(sqlite_store_error)?;
        let mut cursors = std::collections::BTreeMap::new();
        for row in rows {
            let cursor = row.map_err(sqlite_store_error)?;
            cursors.insert(cursor.routine_name.clone(), cursor);
        }
        Ok(cursors)
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
                .map_err(sqlite_store_error)?;
            Ok(inserted > 0)
        })
    }
}
