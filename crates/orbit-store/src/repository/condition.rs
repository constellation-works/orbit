//! Concrete repository for host condition records.

use super::automation::codec::{decode, encode};
use crate::store::{Snapshot, Tx};
use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use orbit_types::workflow::automation::{ConditionAudit, ConditionState};
use rusqlite::Connection;
use rusqlite::{OptionalExtension, params};
use std::marker::PhantomData;
/// Host condition records scoped to a snapshot or write transaction.
pub struct ConditionRepository<'a, Access> {
    conn: &'a Connection,
    _access: PhantomData<&'a Access>,
}
impl<'a, 's> ConditionRepository<'a, Snapshot<'s>> {
    pub(crate) fn for_snapshot(snapshot: &'a Snapshot<'s>) -> Self {
        Self {
            conn: snapshot.connection(),
            _access: PhantomData,
        }
    }
}
impl<'a, 't> ConditionRepository<'a, Tx<'t>> {
    pub(crate) fn for_tx(tx: &'a Tx<'t>) -> Self {
        Self {
            conn: tx.connection(),
            _access: PhantomData,
        }
    }
}

impl<Access> ConditionRepository<'_, Access> {
    pub fn state(&self, consumer: &str) -> Result<Option<ConditionState>, OrbitError> {
        let raw: Option<String> = self
            .conn
            .query_row(
                "SELECT state_json FROM automation_condition_states WHERE consumer=?1",
                [consumer],
                |row| row.get(0),
            )
            .optional()
            .map_err(sqlite_store_error)?;
        raw.map(|raw| decode_state(consumer, &raw)).transpose()
    }

    pub fn states_page(
        &self,
        prefix: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ConditionState>, OrbitError> {
        if limit == 0 {
            return Err(OrbitError::InvalidInput(
                "condition state page limit must be nonzero".into(),
            ));
        }
        {
            let mut stmt = self
                .conn
                .prepare(
                    "SELECT consumer,state_json FROM automation_condition_states \
                 WHERE consumer >= ?1 AND consumer > ?2 \
                 AND substr(consumer,1,length(?1)) = ?1 \
                 ORDER BY consumer LIMIT ?3",
                )
                .map_err(sqlite_store_error)?;
            let rows = stmt
                .query_map(
                    params![prefix, after.unwrap_or(""), limit.min(100)],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .map_err(sqlite_store_error)?;
            rows.map(|row| {
                let (consumer, raw) = row.map_err(sqlite_store_error)?;
                decode_state(&consumer, &raw)
            })
            .collect()
        }
    }

    pub fn audits(&self, consumer: &str, limit: usize) -> Result<Vec<ConditionAudit>, OrbitError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT audit_json FROM automation_condition_audit WHERE consumer=?1 \
                 ORDER BY audit_id DESC LIMIT ?2",
            )
            .map_err(sqlite_store_error)?;
        let rows = stmt
            .query_map(params![consumer, limit.min(100)], |row| {
                row.get::<_, String>(0)
            })
            .map_err(sqlite_store_error)?;
        rows.map(|row| decode(&row.map_err(sqlite_store_error)?))
            .collect()
    }
}
impl ConditionRepository<'_, Tx<'_>> {
    pub fn insert_state(&self, state: &ConditionState) -> Result<bool, OrbitError> {
        self.conn
            .execute(
                "INSERT OR IGNORE INTO automation_condition_states VALUES (?1,0,?2)",
                params![state.consumer, encode(state)?],
            )
            .map(|inserted| inserted == 1)
            .map_err(sqlite_store_error)
    }

    /// Replace a condition row only if the decoded prior row still matches.
    pub fn replace_state(
        &self,
        previous: &ConditionState,
        next: &ConditionState,
    ) -> Result<bool, OrbitError> {
        let conn = self.conn;

        let raw: Option<String> = conn
        .query_row(
            "SELECT state_json FROM automation_condition_states WHERE consumer=?1 AND generation=?2",
            params![previous.consumer, previous.generation],
            |row| row.get(0),
        )
        .optional()
        .map_err(sqlite_store_error)?;
        let Some(raw) = raw else {
            return Ok(false);
        };
        if decode_state(&previous.consumer, &raw)? != *previous {
            return Ok(false);
        }
        conn.execute(
            "UPDATE automation_condition_states SET generation=?1,state_json=?2 \
         WHERE consumer=?3 AND generation=?4 AND state_json=?5",
            params![
                next.generation,
                encode(next)?,
                previous.consumer,
                previous.generation,
                raw
            ],
        )
        .map(|changed| changed == 1)
        .map_err(sqlite_store_error)
    }

    pub fn insert_audit(&self, audit: &ConditionAudit) -> Result<(), OrbitError> {
        self.conn
            .execute(
                "INSERT INTO automation_condition_audit (consumer,at,audit_json) VALUES (?1,?2,?3)",
                params![audit.consumer, audit.at.to_rfc3339(), encode(audit)?],
            )
            .map(drop)
            .map_err(sqlite_store_error)
    }

    pub fn delete_state(&self, consumer: &str) -> Result<(), OrbitError> {
        self.conn
            .execute(
                "DELETE FROM automation_condition_states WHERE consumer=?1",
                [consumer],
            )
            .map(drop)
            .map_err(sqlite_store_error)
    }
}

fn decode_state(consumer: &str, raw: &str) -> Result<ConditionState, OrbitError> {
    let state: ConditionState = decode(raw)?;
    if state.consumer != consumer {
        return Err(OrbitError::Store(
            "persisted condition consumer key does not match its state".into(),
        ));
    }
    state
        .validate()
        .map_err(|error| OrbitError::Store(error.to_string()))?;
    Ok(state)
}
