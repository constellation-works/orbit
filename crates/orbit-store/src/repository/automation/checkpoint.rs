//! Generation- and byte-fenced checkpoint writes.

use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use orbit_types::workflow::automation::AutomationState;
use rusqlite::{Connection, OptionalExtension, params};

use super::codec::{decode, encode};

/// Call inside the write transaction. Match the decoded snapshot, then fence
/// on the bytes actually stored: legacy defaults and JSON formatting need not
/// match the current serializer, but a changed snapshot must still be refused.
pub(super) fn previous_json(
    conn: &Connection,
    previous: &AutomationState,
) -> Result<Option<String>, OrbitError> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT state_json FROM automation_consumers WHERE consumer=?1 AND generation=?2",
            params![previous.consumer, previous.generation],
            |row| row.get(0),
        )
        .optional()
        .map_err(sqlite_store_error)?;

    match raw {
        Some(raw) if decode::<AutomationState>(&raw)? == *previous => Ok(Some(raw)),
        _ => Ok(None),
    }
}

/// Advance inside the write transaction. Fence on the stored bytes after
/// matching the decoded snapshot, so non-canonical JSON does not strand a
/// legacy consumer.
pub(super) fn advance(
    conn: &Connection,
    previous: &AutomationState,
    next: &AutomationState,
) -> Result<bool, OrbitError> {
    let Some(previous_json) = previous_json(conn, previous)? else {
        return Ok(false);
    };

    conn.execute(
        "UPDATE automation_consumers SET generation=?1,state_json=?2 WHERE consumer=?3 AND generation=?4 AND state_json=?5",
        params![
            next.generation,
            encode(next)?,
            previous.consumer,
            previous.generation,
            previous_json
        ],
    )
    .map(|changed| changed == 1)
    .map_err(sqlite_store_error)
}
