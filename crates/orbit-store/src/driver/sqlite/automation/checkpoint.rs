//! Recover the stored bytes for a generation-fenced checkpoint write.

use orbit_common::OrbitError;
use orbit_types::workflow::automation::AutomationState;
use rusqlite::{Connection, OptionalExtension, params};

use super::codec::decode;

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
        .map_err(|e| OrbitError::Store(e.to_string()))?;

    match raw {
        Some(raw) if decode::<AutomationState>(&raw)? == *previous => Ok(Some(raw)),
        _ => Ok(None),
    }
}
