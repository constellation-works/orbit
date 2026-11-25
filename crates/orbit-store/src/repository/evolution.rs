//! Concrete repository for the owner-local evolution ledger
//! (`evolution_records` and `evolution_costs`).
//!
//! Reached only through [`Snapshot::evolution`] (read-only) and
//! [`Tx::evolution`] (read-write inside [`crate::Store::write`]). Cost
//! validation, replay deduplication, signature corroboration, and lifecycle
//! transitions belong to the calling operation; this repository owns SQL
//! queries and row mapping only.

use std::marker::PhantomData;

use orbit_common::{OrbitError, storage::sqlite::sqlite_store_error};
use orbit_types::record::{EvolutionOccurrence, EvolutionRecord};
use rusqlite::{Connection, OptionalExtension, params};

use crate::store::{Snapshot, Tx};

/// Concrete evolution repository scoped to a [`Snapshot`] (read-only) or a
/// [`Tx`] (read-write).
pub struct EvolutionRepository<'a, Access> {
    conn: &'a Connection,
    _access: PhantomData<&'a Access>,
}

impl<'a, 's> EvolutionRepository<'a, Snapshot<'s>> {
    pub(crate) fn for_snapshot(snapshot: &'a Snapshot<'s>) -> Self {
        Self {
            conn: snapshot.connection(),
            _access: PhantomData,
        }
    }
}

impl<'a, 'tx> EvolutionRepository<'a, Tx<'tx>> {
    pub(crate) fn for_tx(tx: &'a Tx<'tx>) -> Self {
        Self {
            conn: tx.connection(),
            _access: PhantomData,
        }
    }

    /// Insert or update an evolution record row.
    pub fn put_record(&self, record: &EvolutionRecord) -> Result<(), OrbitError> {
        let json =
            serde_json::to_string(record).map_err(|error| OrbitError::Store(error.to_string()))?;
        let state = serde_json::to_value(record.state)
            .map_err(|error| OrbitError::Store(error.to_string()))?;
        self.conn
            .execute(
                "INSERT INTO evolution_records(id, workspace_id, signature, state, record_json) \
                 VALUES (?1, ?2, ?3, ?4, ?5) \
                 ON CONFLICT(id) DO UPDATE SET state=excluded.state, record_json=excluded.record_json",
                params![
                    record.id,
                    record.workspace,
                    record.signature,
                    state.as_str(),
                    json
                ],
            )
            .map_err(sqlite_store_error)?;
        Ok(())
    }

    /// Insert one evolution cost occurrence row.
    pub fn put_cost(
        &self,
        workspace_id: &str,
        record_id: Option<&str>,
        event: &EvolutionOccurrence,
    ) -> Result<(), OrbitError> {
        let json =
            serde_json::to_string(event).map_err(|error| OrbitError::Store(error.to_string()))?;
        self.conn
            .execute(
                "INSERT INTO evolution_costs(workspace_id, source_key, record_id, event_json) \
                 VALUES (?1, ?2, ?3, ?4)",
                params![workspace_id, event.source_key, record_id, json],
            )
            .map_err(sqlite_store_error)?;
        Ok(())
    }
}

impl<Access> EvolutionRepository<'_, Access> {
    /// Every evolution record in `workspace_id`, ordered by id.
    pub fn list_records(&self, workspace_id: &str) -> Result<Vec<EvolutionRecord>, OrbitError> {
        let mut stmt = self
            .conn
            .prepare("SELECT record_json FROM evolution_records WHERE workspace_id=?1 ORDER BY id")
            .map_err(sqlite_store_error)?;
        stmt.query_map([workspace_id], |row| row.get::<_, String>(0))
            .map_err(sqlite_store_error)?
            .map(|row| decode_record(row.map_err(sqlite_store_error)?))
            .collect()
    }

    /// One evolution record by `(workspace_id, id)`.
    pub fn get_record(
        &self,
        workspace_id: &str,
        id: &str,
    ) -> Result<Option<EvolutionRecord>, OrbitError> {
        let json: Option<String> = self
            .conn
            .query_row(
                "SELECT record_json FROM evolution_records WHERE workspace_id=?1 AND id=?2",
                params![workspace_id, id],
                |row| row.get(0),
            )
            .optional()
            .map_err(sqlite_store_error)?;
        json.map(decode_record).transpose()
    }

    /// The unresolved (`state <> 'resolved'`) evolution record matching
    /// `(workspace_id, signature)`, if any.
    pub fn get_active_by_signature(
        &self,
        workspace_id: &str,
        signature: &str,
    ) -> Result<Option<EvolutionRecord>, OrbitError> {
        let json: Option<String> = self
            .conn
            .query_row(
                "SELECT record_json FROM evolution_records \
                 WHERE workspace_id=?1 AND signature=?2 AND state <> 'resolved'",
                params![workspace_id, signature],
                |row| row.get(0),
            )
            .optional()
            .map_err(sqlite_store_error)?;
        json.map(decode_record).transpose()
    }

    /// Every cost occurrence in `workspace_id`, ordered by source key.
    pub fn list_costs(&self, workspace_id: &str) -> Result<Vec<EvolutionOccurrence>, OrbitError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT event_json FROM evolution_costs WHERE workspace_id=?1 ORDER BY source_key",
            )
            .map_err(sqlite_store_error)?;
        stmt.query_map([workspace_id], |row| row.get::<_, String>(0))
            .map_err(sqlite_store_error)?
            .map(|row| {
                serde_json::from_str(&row.map_err(sqlite_store_error)?)
                    .map_err(|error| OrbitError::Store(error.to_string()))
            })
            .collect()
    }

    /// Whether a cost occurrence with `(workspace_id, source_key)` is already
    /// stored.
    pub fn cost_exists(&self, workspace_id: &str, source_key: &str) -> Result<bool, OrbitError> {
        self.conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM evolution_costs WHERE workspace_id=?1 AND source_key=?2)",
                params![workspace_id, source_key],
                |row| row.get(0),
            )
            .map_err(sqlite_store_error)
    }
}

fn decode_record(json: String) -> Result<EvolutionRecord, OrbitError> {
    serde_json::from_str(&json)
        .map_err(|error| OrbitError::Store(format!("invalid evolution record: {error}")))
}
