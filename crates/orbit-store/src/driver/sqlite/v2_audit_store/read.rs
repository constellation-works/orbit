use std::collections::HashSet;

use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;

use crate::Store;

use crate::contracts::{V2AuditEventFilter, V2AuditEventRow, V2AuditEventTailPage};

use super::query::{count_matching, run_ids_filter_sql, select_tail_page, v2_filter_sql};
use super::rows::{collect_rows, row_to_v2_audit_event};

#[cfg(test)]
use super::rendezvous::fire_tail_snapshot_rendezvous;

/// Run ids per `IN (...)` list, under SQLite's bound-parameter cap.
const AUDIT_RUN_ID_CHUNK: usize = 500;

impl Store {
    pub fn list_v2_audit_events(
        &self,
        filter: &V2AuditEventFilter,
    ) -> Result<Vec<V2AuditEventRow>, OrbitError> {
        let (where_clause, params) = v2_filter_sql(filter);
        let limit = filter.limit.unwrap_or(1000);
        let offset = filter.offset.unwrap_or(0);
        let order = if filter.oldest_first { "ASC" } else { "DESC" };
        let sql = format!(
            "SELECT id, workspace_id, event_id, source, schema_version, event_type, ts, \
             run_id, agent_identity, parent_event_id, workspace_path, payload_json \
             FROM v2_audit_events {where_clause} ORDER BY ts {order}, id {order} \
             LIMIT ?{} OFFSET ?{}",
            params.len() + 1,
            params.len() + 2
        );
        let mut params = params;
        // Saturate: a wrapped negative LIMIT/OFFSET means "no bound" to SQLite.
        params.push(Box::new(i64::try_from(limit).unwrap_or(i64::MAX)));
        params.push(Box::new(i64::try_from(offset).unwrap_or(i64::MAX)));
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|b| b.as_ref()).collect();

        let conn = self.read()?;
        let mut stmt = conn.prepare(&sql).map_err(sqlite_store_error)?;
        let rows = stmt
            .query_map(param_refs.as_slice(), row_to_v2_audit_event)
            .map_err(sqlite_store_error)?;
        collect_rows(rows)
    }

    pub fn count_v2_audit_events(&self, filter: &V2AuditEventFilter) -> Result<i64, OrbitError> {
        let (where_clause, params) = v2_filter_sql(filter);
        let conn = self.read()?;
        count_matching(&conn, &where_clause, &params)
    }

    /// Filtered total and chronological tail page from one deferred read
    /// transaction on a single pooled reader.
    ///
    /// The snapshot starts when the count runs. A writer that then commits a
    /// row, including one whose timestamp sorts before rows already stored,
    /// stays outside this snapshot: the page ranks match `total`.
    ///
    /// `limit` and `offset` are the tail window (`offset` counts back from the
    /// newest row). The page is read newest-first and reversed, which is the
    /// same window as an oldest-first offset computed from this total.
    /// `oldest_first` is ignored.
    ///
    /// Under `cfg(test)`, the tail-snapshot rendezvous runs after the count
    /// and before the page select. The hook inserts through the writer
    /// connection. A file-backed WAL store lets that commit proceed; the open
    /// reader keeps its snapshot. An in-memory store has no separate reader,
    /// so a hook that re-enters the writer deadlocks — tests must not install
    /// one there.
    pub fn list_v2_audit_event_tail(
        &self,
        filter: &V2AuditEventFilter,
    ) -> Result<V2AuditEventTailPage, OrbitError> {
        let (where_clause, params) = v2_filter_sql(filter);
        let limit = filter.limit.unwrap_or(1000);
        let offset = filter.offset.unwrap_or(0);
        let conn = self.read()?;
        let snapshot = ReadSnapshot::begin(&conn)?;
        let total = count_matching(&conn, &where_clause, &params)?;
        #[cfg(test)]
        fire_tail_snapshot_rendezvous();
        let mut events = select_tail_page(&conn, &where_clause, params, limit, offset)?;
        snapshot.commit()?;
        events.reverse();
        Ok(V2AuditEventTailPage { total, events })
    }

    /// Newest matching rows for each run, independently capped.
    ///
    /// A global `ORDER BY ts DESC LIMIT n` would let a busy earlier run consume
    /// the whole budget. `ROW_NUMBER()` is partitioned by `run_id` so later
    /// runs keep their own newest evidence.
    pub fn list_v2_audit_events_for_runs_partitioned(
        &self,
        workspace_id: &str,
        run_ids: &[String],
        source: Option<&str>,
        body_kind: Option<&str>,
        per_run_limit: usize,
    ) -> Result<Vec<V2AuditEventRow>, OrbitError> {
        if run_ids.is_empty() || per_run_limit == 0 {
            return Ok(Vec::new());
        }
        let mut rows = Vec::new();
        let conn = self.read()?;
        for chunk in run_ids.chunks(AUDIT_RUN_ID_CHUNK) {
            let (where_clause, mut params) =
                run_ids_filter_sql(workspace_id, chunk, source, body_kind, false);
            let limit_idx = params.len() + 1;
            params.push(Box::new(per_run_limit as i64));
            let sql = format!(
                "SELECT id, workspace_id, event_id, source, schema_version, event_type, ts, \
                 run_id, agent_identity, parent_event_id, workspace_path, payload_json \
                 FROM ( \
                    SELECT id, workspace_id, event_id, source, schema_version, event_type, ts, \
                           run_id, agent_identity, parent_event_id, workspace_path, payload_json, \
                           ROW_NUMBER() OVER (PARTITION BY run_id ORDER BY ts DESC, id DESC) AS rn \
                    FROM v2_audit_events {where_clause} \
                 ) WHERE rn <= ?{limit_idx} \
                 ORDER BY ts DESC, id DESC"
            );
            let param_refs: Vec<&dyn rusqlite::types::ToSql> =
                params.iter().map(|b| b.as_ref()).collect();
            let mut stmt = conn.prepare(&sql).map_err(sqlite_store_error)?;
            let mapped = stmt
                .query_map(param_refs.as_slice(), row_to_v2_audit_event)
                .map_err(sqlite_store_error)?;
            rows.extend(collect_rows(mapped)?);
        }
        Ok(rows)
    }

    /// Run ids among `run_ids` that have at least one reconstructable envelope.
    pub fn list_v2_audit_run_ids_with_events(
        &self,
        workspace_id: &str,
        run_ids: &[String],
        source: Option<&str>,
    ) -> Result<HashSet<String>, OrbitError> {
        let mut present = HashSet::new();
        if run_ids.is_empty() {
            return Ok(present);
        }
        let conn = self.read()?;
        for chunk in run_ids.chunks(AUDIT_RUN_ID_CHUNK) {
            let (where_clause, params) =
                run_ids_filter_sql(workspace_id, chunk, source, None, true);
            let sql = format!("SELECT DISTINCT run_id FROM v2_audit_events {where_clause}");
            let param_refs: Vec<&dyn rusqlite::types::ToSql> =
                params.iter().map(|b| b.as_ref()).collect();
            let mut stmt = conn.prepare(&sql).map_err(sqlite_store_error)?;
            let mapped = stmt
                .query_map(param_refs.as_slice(), |row| row.get::<_, String>(0))
                .map_err(sqlite_store_error)?;
            for run_id in mapped {
                present.insert(run_id.map_err(sqlite_store_error)?);
            }
        }
        Ok(present)
    }
}

/// Deferred read transaction on a connection borrowed from [`crate::Store::read`].
///
/// Drop rolls the transaction back unless [`ReadSnapshot::commit`] ran. The
/// pooled reader must not return to the pool with a transaction still open.
struct ReadSnapshot<'a> {
    conn: &'a rusqlite::Connection,
    open: bool,
}

impl<'a> ReadSnapshot<'a> {
    fn begin(conn: &'a rusqlite::Connection) -> Result<Self, OrbitError> {
        conn.execute_batch("BEGIN DEFERRED")
            .map_err(sqlite_store_error)?;
        Ok(Self { conn, open: true })
    }

    fn commit(mut self) -> Result<(), OrbitError> {
        self.conn
            .execute_batch("COMMIT")
            .map_err(sqlite_store_error)?;
        self.open = false;
        Ok(())
    }
}

impl Drop for ReadSnapshot<'_> {
    fn drop(&mut self) {
        if self.open {
            let _ = self.conn.execute_batch("ROLLBACK");
        }
    }
}
