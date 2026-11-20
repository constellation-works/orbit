//! The per-workspace drain intent row (`drain_intents`, schema v95).

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::{OptionalExtension, params};

use crate::Store;
use crate::contracts::{DrainIntentRecord, DrainIntentState};

impl Store {
    /// `workspace_id`'s drain intent; `None` when nothing recorded one. A
    /// row this binary cannot read (an unknown intent token or timestamp)
    /// is an error rather than `off`, so a newer intent is never misreported.
    pub fn drain_intent_for_workspace(
        &self,
        workspace_id: &str,
    ) -> Result<Option<DrainIntentRecord>, OrbitError> {
        let conn = self.read()?;
        let row = conn
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

    /// Replace `workspace_id`'s drain intent with `record`.
    pub fn record_drain_intent_for_workspace(
        &self,
        workspace_id: &str,
        record: &DrainIntentRecord,
    ) -> Result<(), OrbitError> {
        self.with_transaction(|tx| {
            tx.tx
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
        })
    }
}
