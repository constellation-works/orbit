//! Audit-event status counts, duration samples, and hourly buckets.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use rusqlite::params;

use crate::Store;

use super::{param_refs, since_tool_filter, where_clause};

impl Store {
    pub fn get_audit_event_stats(
        &self,
        since: Option<&DateTime<Utc>>,
        tool: Option<&str>,
    ) -> Result<(i64, i64, i64, i64, f64, i64), OrbitError> {
        let conn = self.read()?;

        let (conditions, param_values) = since_tool_filter(since, tool);
        let where_clause = where_clause(&conditions);

        let sql = format!(
            "SELECT \
             COUNT(*), \
             COALESCE(SUM(CASE WHEN status = 'success' THEN 1 ELSE 0 END), 0), \
             COALESCE(SUM(CASE WHEN status = 'failure' THEN 1 ELSE 0 END), 0), \
             COALESCE(SUM(CASE WHEN status = 'denied' THEN 1 ELSE 0 END), 0), \
             COALESCE(AVG(duration_ms), 0.0), \
             COALESCE(MAX(duration_ms), 0) \
             FROM audit_events {where_clause}"
        );

        let param_refs = param_refs(&param_values);

        conn.query_row(&sql, param_refs.as_slice(), |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, f64>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })
        .map_err(sqlite_store_error)
    }

    /// `COUNT(*)` for the same `since` population as [`Self::get_audit_event_stats`].
    ///
    /// A `since` bound is a covering read of `idx_audit_events_timestamp`.
    /// Callers that only need the event denominator use this instead of
    /// loading every duration in the window.
    pub fn get_audit_event_count(&self, since: Option<&DateTime<Utc>>) -> Result<i64, OrbitError> {
        let conn = self.read()?;
        let (conditions, param_values) = since_tool_filter(since, None);
        let where_clause = where_clause(&conditions);
        let sql = format!("SELECT COUNT(*) FROM audit_events {where_clause}");
        let param_refs = param_refs(&param_values);
        conn.query_row(&sql, param_refs.as_slice(), |row| row.get(0))
            .map_err(sqlite_store_error)
    }

    pub fn get_audit_event_durations(
        &self,
        since: Option<&DateTime<Utc>>,
        tool: Option<&str>,
    ) -> Result<Vec<i64>, OrbitError> {
        let conn = self.read()?;

        let (conditions, param_values) = since_tool_filter(since, tool);
        let where_clause = where_clause(&conditions);

        let sql =
            format!("SELECT duration_ms FROM audit_events {where_clause} ORDER BY duration_ms ASC");

        let param_refs = param_refs(&param_values);

        let mut stmt = conn.prepare(&sql).map_err(sqlite_store_error)?;

        let rows = stmt
            .query_map(param_refs.as_slice(), |row| row.get::<_, i64>(0))
            .map_err(sqlite_store_error)?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_store_error)
    }

    /// Returns hourly buckets `(rfc3339_hour_start, count)` of audit events with
    /// `timestamp >= since`, ordered ascending by bucket. Bucket starts are
    /// truncated to `YYYY-MM-DDTHH:00:00Z`. Empty hours are NOT returned —
    /// callers must zero-fill missing hours when rendering a sparkline.
    pub fn get_audit_event_hourly_buckets(
        &self,
        since: &DateTime<Utc>,
    ) -> Result<Vec<(String, i64)>, OrbitError> {
        let conn = self.read()?;

        let sql = "SELECT strftime('%Y-%m-%dT%H:00:00Z', timestamp) AS bucket, COUNT(*) \
                   FROM audit_events WHERE timestamp >= ?1 \
                   GROUP BY bucket ORDER BY bucket ASC";

        let mut stmt = conn.prepare(sql).map_err(sqlite_store_error)?;

        let rows = stmt
            .query_map(params![since.to_rfc3339()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(sqlite_store_error)?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_store_error)
    }
}
