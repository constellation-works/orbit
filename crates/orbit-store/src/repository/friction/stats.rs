//! Aggregate friction projections (ORB-10680).
//!
//! `orbit.friction.stats` used to load the whole corpus and count in Rust.
//! Every counter here is a `GROUP BY` instead, so the rows crossing the
//! boundary are bounded by distinct statuses, models, and tags rather than by
//! record count — and no body is ever read.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::Connection;

use super::queries::encode_timestamp;
use crate::contracts::FrictionReportedCount;

/// Per-status record counts for one workspace.
#[derive(Debug, Clone, Default)]
pub(super) struct StatusCounts {
    pub(super) by_status: BTreeMap<String, u64>,
}

impl StatusCounts {
    pub(super) fn get(&self, status: &str) -> u64 {
        self.by_status.get(status).copied().unwrap_or(0)
    }

    pub(super) fn total(&self) -> u64 {
        self.by_status.values().sum()
    }
}

pub(super) fn status_counts(
    conn: &Connection,
    workspace_id: &str,
) -> Result<StatusCounts, OrbitError> {
    let mut statement = conn
        .prepare(
            "SELECT status, COUNT(*) FROM friction_records
             WHERE workspace_id = ?1 GROUP BY status",
        )
        .map_err(sqlite_store_error)?;
    let rows = statement
        .query_map(rusqlite::params![workspace_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(sqlite_store_error)?;
    let mut by_status = BTreeMap::new();
    for row in rows {
        let (status, count) = row.map_err(sqlite_store_error)?;
        by_status.insert(status, count.max(0) as u64);
    }
    Ok(StatusCounts { by_status })
}

/// Records resolved inside `[start, end)`. A record with no `resolved_at`
/// falls back to `created_at`, matching the retired in-memory aggregation.
pub(super) fn resolved_in_window(
    conn: &Connection,
    workspace_id: &str,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Result<u64, OrbitError> {
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM friction_records
             WHERE workspace_id = ?1 AND status = 'resolved'
               AND COALESCE(resolved_at, created_at) >= ?2
               AND COALESCE(resolved_at, created_at) < ?3",
            rusqlite::params![workspace_id, encode_timestamp(start), encode_timestamp(end)],
            |row| row.get(0),
        )
        .map_err(sqlite_store_error)?;
    Ok(count.max(0) as u64)
}

/// Record counts grouped by reporting model label, optionally windowed.
pub(super) fn counts_by_model(
    conn: &Connection,
    workspace_id: &str,
    since: Option<DateTime<Utc>>,
) -> Result<Vec<FrictionReportedCount>, OrbitError> {
    let (sql, params): (&str, Vec<rusqlite::types::Value>) = match since {
        Some(since) => (
            "SELECT model, COUNT(*) FROM friction_records
             WHERE workspace_id = ?1 AND created_at >= ?2 GROUP BY model",
            vec![
                rusqlite::types::Value::Text(workspace_id.to_string()),
                rusqlite::types::Value::Text(encode_timestamp(since)),
            ],
        ),
        None => (
            "SELECT model, COUNT(*) FROM friction_records
             WHERE workspace_id = ?1 GROUP BY model",
            vec![rusqlite::types::Value::Text(workspace_id.to_string())],
        ),
    };
    let mut statement = conn.prepare(sql).map_err(sqlite_store_error)?;
    let rows = statement
        .query_map(rusqlite::params_from_iter(params.iter()), |row| {
            Ok(FrictionReportedCount {
                model: row.get::<_, String>(0)?,
                count: row.get::<_, i64>(1)?.max(0) as u64,
            })
        })
        .map_err(sqlite_store_error)?;
    rows.map(|row| row.map_err(sqlite_store_error)).collect()
}

/// `(tag, model) -> count`, the input to the per-tag rate table.
pub(super) fn counts_by_tag_and_model(
    conn: &Connection,
    workspace_id: &str,
) -> Result<Vec<(String, String, u64)>, OrbitError> {
    let mut statement = conn
        .prepare(
            "SELECT t.tag, r.model, COUNT(*)
             FROM friction_record_tags t
             JOIN friction_records r
               ON r.workspace_id = t.workspace_id AND r.friction_id = t.friction_id
             WHERE t.workspace_id = ?1
             GROUP BY t.tag, r.model",
        )
        .map_err(sqlite_store_error)?;
    let rows = statement
        .query_map(rusqlite::params![workspace_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?.max(0) as u64,
            ))
        })
        .map_err(sqlite_store_error)?;
    rows.map(|row| row.map_err(sqlite_store_error)).collect()
}
