//! Host-local routine pause and resume.

use std::collections::BTreeMap;

use orbit_common::OrbitError;
use rusqlite::{TransactionBehavior, params};

use super::RoutinePauseRecord;
use crate::Store;

impl Store {
    /// Suppress a routine on this host. Returns `false` when already paused.
    pub fn routine_pause(&self, routine_name: &str, actor: &str) -> Result<bool, OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let now = crate::now_string();
            let inserted = tx
                .tx
                .execute(
                    "INSERT OR IGNORE INTO routine_pauses (routine_name, paused_at, actor)
                     VALUES (?1, ?2, ?3)",
                    params![routine_name, now, actor],
                )
                .map_err(|error| OrbitError::Store(error.to_string()))?;
            Ok(inserted > 0)
        })
    }

    /// Clear a host-local pause. Returns `false` when it was not paused.
    pub fn routine_resume(&self, routine_name: &str) -> Result<bool, OrbitError> {
        self.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
            let deleted = tx
                .tx
                .execute(
                    "DELETE FROM routine_pauses WHERE routine_name = ?1",
                    params![routine_name],
                )
                .map_err(|error| OrbitError::Store(error.to_string()))?;
            Ok(deleted > 0)
        })
    }

    /// All host-local pauses, keyed by routine name.
    pub fn routine_pauses(&self) -> Result<BTreeMap<String, RoutinePauseRecord>, OrbitError> {
        let conn = self.read()?;
        let mut stmt = conn
            .prepare("SELECT routine_name, paused_at, actor FROM routine_pauses")
            .map_err(|error| OrbitError::Store(error.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                Ok(RoutinePauseRecord {
                    routine_name: row.get(0)?,
                    paused_at: row.get(1)?,
                    actor: row.get(2)?,
                })
            })
            .map_err(|error| OrbitError::Store(error.to_string()))?;
        let mut pauses = BTreeMap::new();
        for row in rows {
            let pause = row.map_err(|error| OrbitError::Store(error.to_string()))?;
            pauses.insert(pause.routine_name.clone(), pause);
        }
        Ok(pauses)
    }
}
