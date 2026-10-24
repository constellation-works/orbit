//! Explicit waiver history and removal from scheduling eligibility are atomic.

use super::checkpoint;
use super::codec::{decode, encode};
use crate::Store;
use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use orbit_types::workflow::automation::*;
use rusqlite::{TransactionBehavior, params};

pub(super) fn commit(
    store: &Store,
    previous: &AutomationState,
    next: &AutomationState,
    waiver: &BatchWaiver,
) -> Result<bool, OrbitError> {
    let invalid = || OrbitError::InvalidInput("invalid batch waiver transition".into());
    let active = previous.active.as_ref().ok_or_else(invalid)?;

    // Rebuild the only checkpoint this waiver may produce and require the caller
    // to have proposed exactly it.
    let mut expected = previous.clone();
    expected.generation = expected.generation.checked_add(1).ok_or_else(invalid)?;
    expected.active = None;
    expected.pending.retain(|delivery| {
        !active
            .batch
            .deliveries
            .iter()
            .any(|member| member.key == delivery.key)
    });
    expected.waived.extend(active.batch.deliveries.clone());

    if &expected != next
        || active.batch.id != waiver.batch_id
        || !matches!(active.state, BatchState::Failed | BatchState::Exhausted)
        || waiver.reason.trim().is_empty()
        || waiver.by.trim().is_empty()
    {
        return Err(invalid());
    }

    store.with_transaction_behavior(TransactionBehavior::Immediate, |tx| {
        let conn = tx.connection();

        if !checkpoint::advance(conn, previous, next)? {
            return Ok(false);
        }

        conn.execute(
            "INSERT INTO automation_waivers VALUES (?1,?2,?3,?4)",
            params![
                waiver.batch_id,
                previous.consumer,
                encode(active)?,
                encode(waiver)?
            ],
        )
        .map_err(sqlite_store_error)?;

        Ok(true)
    })
}

pub(super) fn list(
    store: &Store,
    consumer: &str,
    limit: usize,
) -> Result<Vec<BatchWaiver>, OrbitError> {
    store.with_read_connection(|conn| {
        let mut stmt = conn
            .prepare(
                "SELECT waiver_json FROM automation_waivers WHERE consumer=?1 ORDER BY rowid DESC LIMIT ?2",
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
