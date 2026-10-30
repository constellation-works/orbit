//! Condition-trigger state rows and their operator audit. Every write is
//! fenced on the generation and the exact stored bytes, so of two evaluators
//! that loaded one generation only the first commits.

use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use orbit_types::workflow::automation::{ConditionAudit, ConditionState};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use super::codec::{decode, encode};
use crate::Store;

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

pub(super) fn load(store: &Store, consumer: &str) -> Result<Option<ConditionState>, OrbitError> {
    store.with_read_connection(|conn| {
        let raw: Option<String> = conn
            .query_row(
                "SELECT state_json FROM automation_condition_states WHERE consumer=?1",
                [consumer],
                |row| row.get(0),
            )
            .optional()
            .map_err(sqlite_store_error)?;
        raw.map(|raw| decode_state(consumer, &raw)).transpose()
    })
}

pub(super) fn page(
    store: &Store,
    prefix: &str,
    after: Option<&str>,
    limit: usize,
) -> Result<Vec<ConditionState>, OrbitError> {
    if limit == 0 {
        return Err(OrbitError::InvalidInput(
            "condition state page limit must be nonzero".into(),
        ));
    }
    store.with_read_connection(|conn| {
        let mut stmt = conn
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
    })
}

pub(super) fn initialize(store: &Store, state: &ConditionState) -> Result<bool, OrbitError> {
    if state.generation != 0 {
        return Err(OrbitError::InvalidInput(
            "a condition consumer starts at generation 0".into(),
        ));
    }
    state
        .validate()
        .map_err(|error| OrbitError::InvalidInput(error.to_string()))?;
    store.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
        tx.connection()
            .execute(
                "INSERT OR IGNORE INTO automation_condition_states VALUES (?1,0,?2)",
                params![state.consumer, encode(state)?],
            )
            .map(|inserted| inserted == 1)
            .map_err(sqlite_store_error)
    })
}

fn validate_step(previous: &ConditionState, next: &ConditionState) -> Result<(), OrbitError> {
    if previous.consumer != next.consumer {
        return Err(OrbitError::InvalidInput(
            "a condition commit cannot change its consumer".into(),
        ));
    }
    if previous.generation.checked_add(1) != Some(next.generation) {
        return Err(OrbitError::InvalidInput(
            "a condition commit advances exactly one generation".into(),
        ));
    }
    next.validate()
        .map_err(|error| OrbitError::InvalidInput(error.to_string()))?;
    // A fire's identity, captured subject and action key never change once
    // recorded; only its acknowledgement may move.
    for before in &previous.fires {
        if let Some(after) = next.fire(&before.fire_id)
            && (after.subject != before.subject
                || after.revision != before.revision
                || after.arm_sequence != before.arm_sequence
                || after.epoch != before.epoch
                || after.depth != before.depth
                || after.captured != before.captured
                || after.action_key != before.action_key
                || (before.action_id.is_some() && after.action_id != before.action_id))
        {
            return Err(OrbitError::InvalidInput(format!(
                "condition fire {} cannot change its recorded identity or action",
                before.fire_id
            )));
        }
    }
    Ok(())
}

/// Advance inside the write transaction, fenced on generation and bytes.
fn advance(
    conn: &Connection,
    previous: &ConditionState,
    next: &ConditionState,
) -> Result<bool, OrbitError> {
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

pub(super) fn commit(
    store: &Store,
    previous: &ConditionState,
    next: &ConditionState,
) -> Result<bool, OrbitError> {
    validate_step(previous, next)?;
    store.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
        advance(tx.connection(), previous, next)
    })
}

pub(super) fn commit_audited(
    store: &Store,
    previous: &ConditionState,
    next: &ConditionState,
    audit: &ConditionAudit,
) -> Result<bool, OrbitError> {
    validate_step(previous, next)?;
    if audit.consumer != previous.consumer {
        return Err(OrbitError::InvalidInput(
            "a condition audit names the consumer it changes".into(),
        ));
    }
    store.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
        let conn = tx.connection();
        if !advance(conn, previous, next)? {
            return Ok(false);
        }
        conn.execute(
            "INSERT INTO automation_condition_audit (consumer,at,audit_json) VALUES (?1,?2,?3)",
            params![audit.consumer, audit.at.to_rfc3339(), encode(audit)?],
        )
        .map_err(sqlite_store_error)?;
        Ok(true)
    })
}

pub(super) fn audits(
    store: &Store,
    consumer: &str,
    limit: usize,
) -> Result<Vec<ConditionAudit>, OrbitError> {
    store.with_read_connection(|conn| {
        let mut stmt = conn
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
    })
}

pub(super) fn delete(store: &Store, consumer: &str) -> Result<(), OrbitError> {
    store.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
        tx.connection()
            .execute(
                "DELETE FROM automation_condition_states WHERE consumer=?1",
                [consumer],
            )
            .map(drop)
            .map_err(sqlite_store_error)
    })
}
