//! Per-routine baseline and slot cursor.

use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::{OptionalExtension, params};

use super::RoutineCursor;
use super::RoutineRepository;
use crate::Tx;

impl<Access> RoutineRepository<'_, Access> {
    /// Cursor for one routine, if this host has observed it before.
    pub fn cursor(&self, routine_name: &str) -> Result<Option<RoutineCursor>, OrbitError> {
        let conn = self.conn;
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
    pub fn cursors_for(
        &self,
        names: &[String],
    ) -> Result<std::collections::BTreeMap<String, RoutineCursor>, OrbitError> {
        if names.is_empty() {
            return Ok(std::collections::BTreeMap::new());
        }
        let placeholders = vec!["?"; names.len()].join(",");
        let conn = self.conn;
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
}

impl RoutineRepository<'_, Tx<'_>> {
    /// Record the first observation of a routine on this host. Idempotent:
    /// an existing cursor is left untouched, so the baseline never moves
    /// backwards or forwards once set. Returns whether a row was created.
    pub fn insert_cursor(&self, routine_name: &str, baseline_at: &str) -> Result<bool, OrbitError> {
        let now = crate::now_string();
        let inserted = self
            .access
            .connection()
            .execute(
                "INSERT OR IGNORE INTO routine_cursors
                 (routine_name, baseline_at, last_slot, updated_at)
                 VALUES (?1, ?2, NULL, ?3)",
                params![routine_name, baseline_at, now],
            )
            .map_err(sqlite_store_error)?;
        Ok(inserted > 0)
    }
    /// Update the slot on an existing cursor when the incoming slot sorts later.
    /// RFC3339 UTC slot keys sort chronologically.
    pub fn update_cursor_slot(&self, name: &str, slot: &str, now: &str) -> Result<(), OrbitError> {
        self.access
            .connection()
            .execute(
                "UPDATE routine_cursors SET last_slot = ?2, updated_at = ?3
             WHERE routine_name = ?1 AND (last_slot IS NULL OR last_slot < ?2)",
                params![name, slot, now],
            )
            .map_err(sqlite_store_error)?;
        Ok(())
    }
}
