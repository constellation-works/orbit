use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;

use crate::contracts::V2AuditEventRow;
use crate::parse_timestamp;

pub(super) fn row_to_v2_audit_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<V2AuditEventRow> {
    let ts_raw: String = row.get(6)?;
    let schema_version: i64 = row.get(4)?;
    Ok(V2AuditEventRow {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        event_id: row.get(2)?,
        source: row.get(3)?,
        schema_version: schema_version as u32,
        event_type: row.get(5)?,
        ts: parse_timestamp(&ts_raw)?,
        run_id: row.get(7)?,
        agent_identity: row.get(8)?,
        parent_event_id: row.get(9)?,
        workspace_path: row.get(10)?,
        payload_json: row.get(11)?,
    })
}

pub(super) fn collect_rows<T>(
    rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>>,
) -> Result<Vec<T>, OrbitError> {
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_store_error)
}
