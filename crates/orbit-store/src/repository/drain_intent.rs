//! Concrete repository for the per-workspace drain intent row
//! (`drain_intents`, schema v95).
//!
//! Reached only through [`Snapshot::drain_intent`] and [`Tx::drain_intent`].

use std::marker::PhantomData;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::{Connection, OptionalExtension, params};

use crate::contract::{DrainIntentRecord, DrainIntentState};
use crate::store::{Snapshot, Tx};

/// Concrete drain-intent repository scoped to a [`Snapshot`] (read-only) or
/// a [`Tx`] (read-write).
pub struct DrainIntentRepository<'a, Access> {
    conn: &'a Connection,
    _access: PhantomData<&'a Access>,
}

impl<'a, 's> DrainIntentRepository<'a, Snapshot<'s>> {
    pub(crate) fn for_snapshot(snapshot: &'a Snapshot<'s>) -> Self {
        Self {
            conn: snapshot.connection(),
            _access: PhantomData,
        }
    }
}

impl<'a, 'tx> DrainIntentRepository<'a, Tx<'tx>> {
    pub(crate) fn for_tx(tx: &'a Tx<'tx>) -> Self {
        Self {
            conn: tx.connection(),
            _access: PhantomData,
        }
    }

    /// Replace `workspace_id`'s drain intent with `record`.
    pub fn put(&self, workspace_id: &str, record: &DrainIntentRecord) -> Result<(), OrbitError> {
        self.conn
            .execute(
                "INSERT INTO drain_intents \
                 (workspace_id, intent, actor, source, reason, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
                 ON CONFLICT(workspace_id) DO UPDATE SET \
                 intent = excluded.intent, actor = excluded.actor, \
                 source = excluded.source, reason = excluded.reason, \
                 updated_at = excluded.updated_at",
                params![
                    workspace_id,
                    record.intent.as_str(),
                    record.actor,
                    record.source,
                    record.reason,
                    record
                        .updated_at
                        .to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
                ],
            )
            .map_err(sqlite_store_error)?;
        Ok(())
    }
}

impl<Access> DrainIntentRepository<'_, Access> {
    /// `workspace_id`'s drain intent; `None` when nothing recorded one. A
    /// row this binary cannot read (an unknown intent token or timestamp)
    /// is an error rather than `off`, so a newer intent is never misreported.
    pub fn get(&self, workspace_id: &str) -> Result<Option<DrainIntentRecord>, OrbitError> {
        let row = self
            .conn
            .query_row(
                "SELECT intent, actor, source, reason, updated_at FROM drain_intents \
                 WHERE workspace_id = ?1",
                params![workspace_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()
            .map_err(sqlite_store_error)?;
        let Some((intent, actor, source, reason, updated_at)) = row else {
            return Ok(None);
        };
        let intent = DrainIntentState::parse_stored(&intent)
            .ok_or_else(|| OrbitError::Store(format!("unknown drain intent '{intent}'")))?;
        let updated_at = DateTime::parse_from_rfc3339(&updated_at)
            .map_err(|error| OrbitError::Store(format!("invalid drain intent time: {error}")))?
            .with_timezone(&Utc);
        Ok(Some(DrainIntentRecord {
            intent,
            actor,
            source,
            reason,
            updated_at,
        }))
    }
}
