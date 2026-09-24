//! Friction allocation and publication inside a caller-owned SQLite transaction.
use crate::contracts::FrictionAddParams;
use chrono::{DateTime, SecondsFormat, Utc};
use orbit_common::{OrbitError, governance::friction::derive_title};
use orbit_types::record::{FrictionRecord, FrictionStatus};
use rusqlite::Connection;

fn encode_timestamp(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Nanos, true)
}

/// Writes the record row and replaces its denormalized tag rows.
///
/// `friction_record_tags` exists so a tag filter is an index probe instead of
/// a JSON scan; `tags_json` stays the ordered source of truth for the record's
/// own projection.
pub(crate) fn upsert_record(
    conn: &Connection,
    workspace_id: &str,
    record: &FrictionRecord,
    month: &str,
    seq: u32,
    legacy_path: Option<&str>,
) -> Result<(), OrbitError> {
    let tags_json = serde_json::to_string(&record.tags)
        .map_err(|error| OrbitError::Store(format!("serialize friction tags: {error}")))?;
    conn.execute(
        "INSERT INTO friction_records (
             workspace_id, friction_id, month, seq, title, model, status, created_at,
             resolved_at, during_task, resolved_by_task, tags_json, body, legacy_path
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
         ON CONFLICT(workspace_id, friction_id) DO UPDATE SET
             title = excluded.title,
             model = excluded.model,
             status = excluded.status,
             created_at = excluded.created_at,
             resolved_at = excluded.resolved_at,
             during_task = excluded.during_task,
             resolved_by_task = excluded.resolved_by_task,
             tags_json = excluded.tags_json,
             body = excluded.body",
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
        ],
    )
    .map_err(|error| OrbitError::Store(format!("write friction record {}: {error}", record.id)))?;

    conn.execute(
        "DELETE FROM friction_record_tags WHERE workspace_id = ?1 AND friction_id = ?2",
        rusqlite::params![workspace_id, record.id],
    )
    .map_err(|error| OrbitError::Store(error.to_string()))?;
    for tag in &record.tags {
        conn.execute(
            "INSERT OR IGNORE INTO friction_record_tags (workspace_id, friction_id, tag)
             VALUES (?1, ?2, ?3)",
            rusqlite::params![workspace_id, record.id, tag],
        )
        .map_err(|error| OrbitError::Store(error.to_string()))?;
    }
    Ok(())
}

/// Highest per-month counter the `FYYYY-MM-NNN` ID grammar can carry.
const MAX_MONTH_SEQ: i64 = 999;

/// Next per-month counter for a workspace. Callers must hold the write
/// transaction so the read and the matching insert cannot interleave.
///
/// Refuses to allocate past [`MAX_MONTH_SEQ`]: a four-digit suffix would be
/// stored but rejected by every ID-taking read and write path afterwards.
pub(crate) fn next_month_seq(
    conn: &Connection,
    workspace_id: &str,
    month: &str,
) -> Result<u32, OrbitError> {
    let next: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM friction_records
             WHERE workspace_id = ?1 AND month = ?2",
            rusqlite::params![workspace_id, month],
            |row| row.get(0),
        )
        .map_err(|error| OrbitError::Store(error.to_string()))?;
    if next > MAX_MONTH_SEQ {
        return Err(OrbitError::InvalidInput(format!(
            "friction log for {month} is full ({MAX_MONTH_SEQ} records); \
             resolve or consolidate existing records before adding more"
        )));
    }
    u32::try_from(next).map_err(|_| {
        OrbitError::Store(format!(
            "friction counter for workspace '{workspace_id}' month '{month}' overflowed"
        ))
    })
}

/// Allocate within the caller's existing transaction. The claim journal uses
/// this seam so revocation, replay receipts and friction publication cannot
/// interleave. Taxonomy normalization must precede this call.
pub(crate) fn add_in_transaction(
    conn: &rusqlite::Connection,
    workspace_id: &str,
    params: &FrictionAddParams,
) -> Result<FrictionRecord, OrbitError> {
    if params.model.trim().is_empty() {
        return Err(OrbitError::InvalidInput(
            "friction model must not be empty".into(),
        ));
    }
    let month = params.created_at.format("%Y-%m").to_string();
    let seq = next_month_seq(conn, workspace_id, &month)?;
    let record = FrictionRecord {
        id: format!("F{month}-{seq:03}"),
        title: params.title.clone().or_else(|| derive_title(&params.body)),
        model: params.model.trim().into(),
        created_at: params.created_at,
        status: FrictionStatus::Open,
        tags: params.tags.clone(),
        resolved_at: None,
        during_task: params.during_task.clone(),
        resolved_by_task: None,
        body: params.body.clone(),
    };
    upsert_record(conn, workspace_id, &record, &month, seq, None)?;
    Ok(record)
}
