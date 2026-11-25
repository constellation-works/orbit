//! SQL writes for friction records and their indexed tags.

use orbit_common::storage::sqlite::{sqlite_error, sqlite_store_error};
use orbit_common::{OrbitError, StorageLayer};
use orbit_types::record::FrictionRecord;

use super::queries::encode_timestamp;
use crate::Tx;

/// Writes the record row and replaces its denormalized tag rows.
///
/// `friction_record_tags` exists so a tag filter is an index probe instead of
/// a JSON scan; `tags_json` stays the ordered source of truth for the record's
/// own projection.
pub(super) fn upsert_record(
    tx: &Tx<'_>,
    workspace_id: &str,
    record: &FrictionRecord,
    month: &str,
    seq: u32,
    legacy_path: Option<&str>,
) -> Result<(), OrbitError> {
    let conn = tx.connection();
    let tags_json = serde_json::to_string(&record.tags)
        .map_err(|error| OrbitError::Store(format!("serialize friction tags: {error}")))?;
    conn.execute(
        "INSERT INTO friction_records (
             workspace_id, friction_id, month, seq, title, model, status, created_at,
             resolved_at, during_task, resolved_by_task, tags_json, body, legacy_path,
             rehome_to
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
         ON CONFLICT(workspace_id, friction_id) DO UPDATE SET
             title = excluded.title,
             model = excluded.model,
             status = excluded.status,
             created_at = excluded.created_at,
             resolved_at = excluded.resolved_at,
             during_task = excluded.during_task,
             resolved_by_task = excluded.resolved_by_task,
             tags_json = excluded.tags_json,
             body = excluded.body,
             rehome_to = excluded.rehome_to",
        rusqlite::params![
            workspace_id,
            record.id,
            month,
            seq,
            record.title,
            record.model,
            record.status.as_str(),
            encode_timestamp(record.created_at),
            record.resolved_at.map(encode_timestamp),
            record.during_task,
            record.resolved_by_task,
            tags_json,
            record.body,
            legacy_path,
            record.rehome_to,
        ],
    )
    .map_err(|error| {
        sqlite_error(
            StorageLayer::Store,
            &error,
            format!("write friction record {}: {error}", record.id),
        )
    })?;

    conn.execute(
        "DELETE FROM friction_record_tags WHERE workspace_id = ?1 AND friction_id = ?2",
        rusqlite::params![workspace_id, record.id],
    )
    .map_err(sqlite_store_error)?;
    for tag in &record.tags {
        conn.execute(
            "INSERT OR IGNORE INTO friction_record_tags (workspace_id, friction_id, tag)
             VALUES (?1, ?2, ?3)",
            rusqlite::params![workspace_id, record.id, tag],
        )
        .map_err(sqlite_store_error)?;
    }
    Ok(())
}

/// Next per-month counter for a workspace. The write transaction prevents
/// another allocator from inserting between this query and its matching put.
pub(super) fn next_month_seq(
    tx: &Tx<'_>,
    workspace_id: &str,
    month: &str,
) -> Result<i64, OrbitError> {
    tx.connection()
        .query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM friction_records
             WHERE workspace_id = ?1 AND month = ?2",
            rusqlite::params![workspace_id, month],
            |row| row.get(0),
        )
        .map_err(sqlite_store_error)
}
