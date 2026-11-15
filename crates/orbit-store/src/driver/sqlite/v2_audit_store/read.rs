use std::collections::{HashMap, HashSet};

use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;

use crate::Store;

use crate::contracts::{
    V2AuditEventFilter, V2AuditEventRow, V2AuditEventTailPage, V2AuditKindForwardPayloads,
    V2AuditKindForwardQuery,
};

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
                run_ids_filter_sql(workspace_id, chunk, source, body_kind, true);
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

    /// Oldest `scan_limit` rows for one run, payloads only where `event_type`
    /// cannot reject the requested kind, plus whether another raw row exists.
    ///
    /// The id window and the past-limit probe share one deferred read
    /// snapshot. Payload text is a later lookup by those ids, so a window
    /// whose event types are all known non-matches reads none.
    pub fn list_v2_audit_kind_forward_payloads(
        &self,
        query: &V2AuditKindForwardQuery<'_>,
    ) -> Result<V2AuditKindForwardPayloads, OrbitError> {
        let scan_limit = i64::try_from(query.scan_limit).unwrap_or(i64::MAX);
        let conn = self.read()?;
        let snapshot = ReadSnapshot::begin(&conn)?;
        let past_scan_limit = kind_window_has_row_past_limit(
            &conn,
            query.workspace_id,
            query.run_id,
            query.source,
            scan_limit,
        )?;
        let ids = kind_window_candidate_ids(
            &conn,
            query.workspace_id,
            query.run_id,
            query.source,
            scan_limit,
            query.canonical_event_type,
            query.known_event_types,
        )?;
        let payloads = payloads_for_ids(&conn, &ids)?;
        snapshot.commit()?;
        Ok(V2AuditKindForwardPayloads {
            payloads,
            past_scan_limit,
        })
    }
}

fn kind_window_has_row_past_limit(
    conn: &rusqlite::Connection,
    workspace_id: &str,
    run_id: &str,
    source: &str,
    scan_limit: i64,
) -> Result<bool, OrbitError> {
    let sql = "SELECT 1 FROM v2_audit_events \
               WHERE workspace_id = ?1 AND run_id = ?2 AND source = ?3 \
               ORDER BY ts ASC, id ASC LIMIT 1 OFFSET ?4";
    let present = conn
        .query_row(sql, (workspace_id, run_id, source, scan_limit), |_| Ok(()))
        .optional_row()?;
    Ok(present.is_some())
}

fn kind_window_candidate_ids(
    conn: &rusqlite::Connection,
    workspace_id: &str,
    run_id: &str,
    source: &str,
    scan_limit: i64,
    canonical_event_type: Option<&str>,
    known_event_types: &[&str],
) -> Result<Vec<i64>, OrbitError> {
    let mut sql = String::from(
        "WITH window AS ( \
            SELECT id, event_type, ts FROM v2_audit_events \
            WHERE workspace_id = ?1 AND run_id = ?2 AND source = ?3 \
            ORDER BY ts ASC, id ASC LIMIT ?4 \
         ) SELECT id FROM window",
    );
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = vec![
        Box::new(workspace_id.to_string()),
        Box::new(run_id.to_string()),
        Box::new(source.to_string()),
        Box::new(scan_limit),
    ];
    if let Some(canonical) = canonical_event_type.filter(|_| !known_event_types.is_empty()) {
        params.push(Box::new(canonical.to_string()));
        let canonical_index = params.len();
        let mut placeholders = Vec::with_capacity(known_event_types.len());
        for event_type in known_event_types {
            params.push(Box::new((*event_type).to_string()));
            placeholders.push(format!("?{}", params.len()));
        }
        sql.push_str(&format!(
            " WHERE event_type = ?{canonical_index} OR event_type NOT IN ({})",
            placeholders.join(", ")
        ));
    }
    sql.push_str(" ORDER BY ts ASC, id ASC");
    let refs: Vec<&dyn rusqlite::types::ToSql> =
        params.iter().map(|param| param.as_ref()).collect();
    let mut stmt = conn.prepare(&sql).map_err(sqlite_store_error)?;
    let rows = stmt
        .query_map(refs.as_slice(), |row| row.get::<_, i64>(0))
        .map_err(sqlite_store_error)?;
    let mut ids = Vec::new();
    for id in rows {
        ids.push(id.map_err(sqlite_store_error)?);
    }
    Ok(ids)
}

fn payloads_for_ids(conn: &rusqlite::Connection, ids: &[i64]) -> Result<Vec<String>, OrbitError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut by_id = HashMap::with_capacity(ids.len());
    for chunk in ids.chunks(AUDIT_RUN_ID_CHUNK) {
        let mut sql = String::from("SELECT id, payload_json FROM v2_audit_events WHERE id IN (");
        let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::with_capacity(chunk.len());
        for (index, id) in chunk.iter().enumerate() {
            if index > 0 {
                sql.push_str(", ");
            }
            params.push(Box::new(*id));
            sql.push('?');
            sql.push_str(&(params.len()).to_string());
        }
        sql.push(')');
        let refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|param| param.as_ref()).collect();
        let mut stmt = conn.prepare(&sql).map_err(sqlite_store_error)?;
        let rows = stmt
            .query_map(refs.as_slice(), |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(sqlite_store_error)?;
        for row in rows {
            let (id, payload) = row.map_err(sqlite_store_error)?;
            by_id.insert(id, payload);
        }
    }
    let mut payloads = Vec::with_capacity(ids.len());
    for id in ids {
        let Some(payload) = by_id.remove(id) else {
            return Err(OrbitError::Store(format!(
                "v2 audit kind page missed row {id}"
            )));
        };
        payloads.push(payload);
    }
    Ok(payloads)
}

trait OptionalQueryRow {
    fn optional_row(self) -> Result<Option<()>, OrbitError>;
}

impl OptionalQueryRow for Result<(), rusqlite::Error> {
    fn optional_row(self) -> Result<Option<()>, OrbitError> {
        match self {
            Ok(()) => Ok(Some(())),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(sqlite_store_error(error)),
        }
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
