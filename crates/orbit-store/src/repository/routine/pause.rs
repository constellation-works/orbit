//! Host-local routine pause and resume.

use std::collections::BTreeMap;

use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::params;

use super::RoutinePauseRecord;
use super::RoutineRepository;
use crate::Tx;

impl<Access> RoutineRepository<'_, Access> {
    /// All host-local pauses, keyed by routine name.
    pub fn pauses(&self) -> Result<BTreeMap<String, RoutinePauseRecord>, OrbitError> {
        let conn = self.conn;
        let mut stmt = conn
            .prepare("SELECT routine_name, paused_at, actor FROM routine_pauses")
            .map_err(sqlite_store_error)?;
        let rows = stmt
            .query_map([], |row| {
                Ok(RoutinePauseRecord {
                    routine_name: row.get(0)?,
                    paused_at: row.get(1)?,
                    actor: row.get(2)?,
                })
            })
            .map_err(sqlite_store_error)?;
        let mut pauses = BTreeMap::new();
        for row in rows {
            let pause = row.map_err(sqlite_store_error)?;
            pauses.insert(pause.routine_name.clone(), pause);
        }
        Ok(pauses)
    }
}

impl RoutineRepository<'_, Tx<'_>> {
    /// Suppress a routine on this host. Returns `false` when already paused.
    pub fn insert_pause(&self, routine_name: &str, actor: &str) -> Result<bool, OrbitError> {
        let now = crate::now_string();
        let inserted = self
            .access
            .connection()
            .execute(
                "INSERT OR IGNORE INTO routine_pauses (routine_name, paused_at, actor)
                 VALUES (?1, ?2, ?3)",
                params![routine_name, now, actor],
            )
            .map_err(sqlite_store_error)?;
        Ok(inserted > 0)
    }

    /// Clear a host-local pause. Returns `false` when it was not paused.
    pub fn delete_pause(&self, routine_name: &str) -> Result<bool, OrbitError> {
        let deleted = self
            .access
            .connection()
            .execute(
                "DELETE FROM routine_pauses WHERE routine_name = ?1",
                params![routine_name],
            )
            .map_err(sqlite_store_error)?;
        Ok(deleted > 0)
    }
}
