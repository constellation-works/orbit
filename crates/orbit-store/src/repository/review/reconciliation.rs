//! Reconciliation row queries and revision fences.
use super::ledger::{decode, encode};
use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use orbit_types::workflow::ReviewReconciliation;
use rusqlite::{Connection, OptionalExtension, params};

pub(super) fn read(
    conn: &Connection,
    workspace_id: &str,
    reconciliation_id: &str,
) -> Result<Option<ReviewReconciliation>, OrbitError> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT record_json FROM review_reconciliations WHERE workspace_id=?1 AND reconciliation_id=?2",
            params![workspace_id, reconciliation_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(sqlite_store_error)?;
    raw.as_deref().map(decode).transpose()
}

pub(super) fn for_request(
    conn: &Connection,
    workspace_id: &str,
    task_id: &str,
    request_key: &str,
) -> Result<Option<ReviewReconciliation>, OrbitError> {
    let existing: Option<String> = conn
                .query_row(
                    "SELECT record_json FROM review_reconciliations WHERE workspace_id=?1 AND task_id=?2 AND request_key=?3",
                    params![workspace_id, task_id, request_key],
                    |row| row.get(0),
                )
                .optional()
                .map_err(sqlite_store_error)?;

    existing.as_deref().map(decode).transpose()
}

pub(super) fn insert(
    conn: &Connection,
    workspace_id: &str,
    record: &ReviewReconciliation,
) -> Result<(), OrbitError> {
    conn.execute(
                "INSERT INTO review_reconciliations (workspace_id, reconciliation_id, task_id, request_key, revision, record_json, updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![
                    workspace_id,
                    record.reconciliation_id,
                    record.binding.task_id,
                    record.request_key,
                    record.revision,
                    encode(record)?,
                    record.updated_at.to_rfc3339(),
                ],
            )
            .map_err(sqlite_store_error)?;

    Ok(())
}

pub(super) fn replace(
    conn: &Connection,
    workspace_id: &str,
    revision: u32,
    record: &ReviewReconciliation,
) -> Result<(), OrbitError> {
    let changed = conn
                .execute(
                    "UPDATE review_reconciliations SET revision=?1, record_json=?2, updated_at=?3 WHERE workspace_id=?4 AND reconciliation_id=?5 AND revision=?6",
                    params![
                        record.revision,
                        encode(record)?,
                        record.updated_at.to_rfc3339(),
                        workspace_id,
                        record.reconciliation_id,
                        revision,
                    ],
                )
                .map_err(sqlite_store_error)?;
    if changed == 0 {
        return Err(OrbitError::Store(
            "review reconciliation changed concurrently; reread and retry".into(),
        ));
    }

    Ok(())
}
