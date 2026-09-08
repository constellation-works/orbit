use std::collections::HashSet;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;

use crate::{Store, parse_timestamp};

use crate::contracts::{V2AuditEventFilter, V2AuditEventInsertParams, V2AuditEventRow};

/// Run ids per `IN (...)` list, under SQLite's bound-parameter cap.
const AUDIT_RUN_ID_CHUNK: usize = 500;

impl Store {
    pub fn insert_v2_audit_event(
        &self,
        params: &V2AuditEventInsertParams,
    ) -> Result<(), OrbitError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| OrbitError::Store(format!("mutex poisoned: {e}")))?;
        conn.execute(
            r#"INSERT OR IGNORE INTO v2_audit_events(
                workspace_id, event_id, source, schema_version, event_type, ts,
                run_id, agent_identity, parent_event_id, workspace_path, payload_json
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)"#,
            rusqlite::params![
                params.workspace_id,
                params.event_id,
                params.source,
                i64::from(params.schema_version),
                params.event_type,
                params.ts.to_rfc3339(),
                params.run_id,
                params.agent_identity,
                params.parent_event_id,
                params.workspace_path,
                params.payload_json,
            ],
        )
        .map_err(|e| OrbitError::Store(e.to_string()))?;
        Ok(())
    }

    pub fn list_v2_audit_events(
        &self,
        filter: &V2AuditEventFilter,
    ) -> Result<Vec<V2AuditEventRow>, OrbitError> {
        let (where_clause, params) = v2_filter_sql(filter);
        let limit = filter.limit.unwrap_or(1000);
        let offset = filter.offset.unwrap_or(0);
        let sql = format!(
            "SELECT id, workspace_id, event_id, source, schema_version, event_type, ts, \
             run_id, agent_identity, parent_event_id, workspace_path, payload_json \
             FROM v2_audit_events {where_clause} ORDER BY ts DESC, id DESC \
             LIMIT ?{} OFFSET ?{}",
            params.len() + 1,
            params.len() + 2
        );
        let mut params = params;
        params.push(Box::new(limit as i64));
        params.push(Box::new(offset as i64));
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|b| b.as_ref()).collect();

        let conn = self.read()?;
        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        let rows = stmt
            .query_map(param_refs.as_slice(), row_to_v2_audit_event)
            .map_err(|e| OrbitError::Store(e.to_string()))?;
        collect_rows(rows)
    }

    pub fn count_v2_audit_events(&self, filter: &V2AuditEventFilter) -> Result<i64, OrbitError> {
        let (where_clause, params) = v2_filter_sql(filter);
        let sql = format!("SELECT COUNT(*) FROM v2_audit_events {where_clause}");
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|b| b.as_ref()).collect();
        let conn = self.read()?;
        conn.query_row(&sql, param_refs.as_slice(), |row| row.get(0))
            .map_err(|e| OrbitError::Store(e.to_string()))
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
            let mut stmt = conn
                .prepare(&sql)
                .map_err(|e| OrbitError::Store(e.to_string()))?;
            let mapped = stmt
                .query_map(param_refs.as_slice(), row_to_v2_audit_event)
                .map_err(|e| OrbitError::Store(e.to_string()))?;
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
            let mut stmt = conn
                .prepare(&sql)
                .map_err(|e| OrbitError::Store(e.to_string()))?;
            let mapped = stmt
                .query_map(param_refs.as_slice(), |row| row.get::<_, String>(0))
                .map_err(|e| OrbitError::Store(e.to_string()))?;
            for run_id in mapped {
                present.insert(run_id.map_err(|e| OrbitError::Store(e.to_string()))?);
            }
        }
        Ok(present)
    }

    pub fn prune_v2_audit_events_older_than(
        &self,
        workspace_id: &str,
        ts: &DateTime<Utc>,
    ) -> Result<usize, OrbitError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| OrbitError::Store(format!("mutex poisoned: {e}")))?;
        conn.execute(
            "DELETE FROM v2_audit_events WHERE workspace_id = ?1 AND ts < ?2",
            rusqlite::params![workspace_id, ts.to_rfc3339()],
        )
        .map_err(|e| OrbitError::Store(e.to_string()))
    }
}

impl crate::contracts::V2AuditStoreBackend for Store {
    fn insert_v2_audit_event(&self, params: &V2AuditEventInsertParams) -> Result<(), OrbitError> {
        Self::insert_v2_audit_event(self, params)
    }

    fn list_v2_audit_events(
        &self,
        filter: &V2AuditEventFilter,
    ) -> Result<Vec<V2AuditEventRow>, OrbitError> {
        Self::list_v2_audit_events(self, filter)
    }

    fn count_v2_audit_events(&self, filter: &V2AuditEventFilter) -> Result<i64, OrbitError> {
        Self::count_v2_audit_events(self, filter)
    }

    fn list_v2_audit_events_for_runs_partitioned(
        &self,
        workspace_id: &str,
        run_ids: &[String],
        source: Option<&str>,
        body_kind: Option<&str>,
        per_run_limit: usize,
    ) -> Result<Vec<V2AuditEventRow>, OrbitError> {
        Self::list_v2_audit_events_for_runs_partitioned(
            self,
            workspace_id,
            run_ids,
            source,
            body_kind,
            per_run_limit,
        )
    }

    fn list_v2_audit_run_ids_with_events(
        &self,
        workspace_id: &str,
        run_ids: &[String],
        source: Option<&str>,
    ) -> Result<HashSet<String>, OrbitError> {
        Self::list_v2_audit_run_ids_with_events(self, workspace_id, run_ids, source)
    }
}

fn run_ids_filter_sql(
    workspace_id: &str,
    run_ids: &[String],
    source: Option<&str>,
    body_kind: Option<&str>,
    require_event_id: bool,
) -> (String, Vec<Box<dyn rusqlite::types::ToSql>>) {
    let mut conditions = vec!["workspace_id = ?1".to_string()];
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(workspace_id.to_string())];
    if let Some(source) = source {
        conditions.push(format!("source = ?{}", params.len() + 1));
        params.push(Box::new(source.to_string()));
    }
    if body_kind.is_some() || require_event_id {
        // `json_extract` errors on malformed payloads; skip them the same way
        // the per-run reconstruction skips unparseable envelope rows.
        conditions.push("json_valid(payload_json)".to_string());
    }
    if let Some(body_kind) = body_kind {
        conditions.push(format!(
            "json_extract(payload_json, '$.body_kind') = ?{}",
            params.len() + 1
        ));
        params.push(Box::new(body_kind.to_string()));
    }
    if require_event_id {
        conditions.push("json_extract(payload_json, '$.event_id') IS NOT NULL".to_string());
    }
    let start = params.len() + 1;
    let placeholders = (0..run_ids.len())
        .map(|index| format!("?{}", start + index))
        .collect::<Vec<_>>()
        .join(", ");
    conditions.push(format!("run_id IN ({placeholders})"));
    for run_id in run_ids {
        params.push(Box::new(run_id.clone()));
    }
    (format!("WHERE {}", conditions.join(" AND ")), params)
}

fn v2_filter_sql(filter: &V2AuditEventFilter) -> (String, Vec<Box<dyn rusqlite::types::ToSql>>) {
    let mut conditions = vec!["workspace_id = ?1".to_string()];
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> =
        vec![Box::new(filter.workspace_id.clone())];
    if let Some(since) = filter.since {
        conditions.push(format!("ts >= ?{}", params.len() + 1));
        params.push(Box::new(since.to_rfc3339()));
    }
    if let Some(until) = filter.until {
        conditions.push(format!("ts <= ?{}", params.len() + 1));
        params.push(Box::new(until.to_rfc3339()));
    }
    if let Some(run_id) = &filter.run_id {
        conditions.push(format!("run_id = ?{}", params.len() + 1));
        params.push(Box::new(run_id.clone()));
    }
    if let Some(event_type) = &filter.event_type {
        conditions.push(format!("event_type = ?{}", params.len() + 1));
        params.push(Box::new(event_type.clone()));
    }
    if let Some(source) = &filter.source {
        conditions.push(format!("source = ?{}", params.len() + 1));
        params.push(Box::new(source.clone()));
    }
    (format!("WHERE {}", conditions.join(" AND ")), params)
}

fn row_to_v2_audit_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<V2AuditEventRow> {
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

fn collect_rows<T>(
    rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>>,
) -> Result<Vec<T>, OrbitError> {
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| OrbitError::Store(e.to_string()))
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;

    #[test]
    fn insert_list_and_count_are_workspace_scoped() {
        let store = Store::open_in_memory().expect("store");
        let ts = Utc::now();
        for workspace_id in ["ws_a", "ws_b"] {
            store
                .insert_v2_audit_event(&V2AuditEventInsertParams {
                    workspace_id: workspace_id.to_string(),
                    event_id: format!("evt-{workspace_id}"),
                    source: "v2_envelope".to_string(),
                    schema_version: 1,
                    event_type: "tool.denied".to_string(),
                    ts,
                    run_id: "run-1".to_string(),
                    agent_identity: "codex".to_string(),
                    parent_event_id: None,
                    workspace_path: None,
                    payload_json: "{}".to_string(),
                })
                .expect("insert");
        }

        let filter = V2AuditEventFilter {
            workspace_id: "ws_a".to_string(),
            event_type: Some("tool.denied".to_string()),
            ..Default::default()
        };
        let rows = store.list_v2_audit_events(&filter).expect("list");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].event_id, "evt-ws_a");
        assert_eq!(store.count_v2_audit_events(&filter).expect("count"), 1);
    }

    /// [ORB-11625] Per-run partition limits keep a busy earlier run from
    /// starving a later one. Presence ignores malformed payloads.
    #[test]
    fn partitioned_recovery_reads_do_not_starve_later_runs() {
        let store = Store::open_in_memory().expect("store");
        let busy = "jrun-busy";
        let quiet = "jrun-quiet";
        let empty = "jrun-empty";
        for index in 0..12_u32 {
            insert_envelope(
                &store,
                busy,
                &format!("evt-busy-{index}"),
                index,
                Some("step_recovery_attempted"),
            );
        }
        insert_envelope(&store, quiet, "evt-quiet-0", 0, Some("step_started"));
        insert_envelope(
            &store,
            quiet,
            "evt-quiet-1",
            1,
            Some("step_recovery_attempted"),
        );
        store
            .insert_v2_audit_event(&V2AuditEventInsertParams {
                workspace_id: "ws_a".to_string(),
                event_id: "evt-malformed".to_string(),
                source: "v2_envelope".to_string(),
                schema_version: 1,
                event_type: "test.event".to_string(),
                ts: Utc::now(),
                run_id: empty.to_string(),
                agent_identity: "codex".to_string(),
                parent_event_id: None,
                workspace_path: None,
                payload_json: "{not-json".to_string(),
            })
            .expect("insert malformed");

        let run_ids = vec![busy.to_string(), quiet.to_string(), empty.to_string()];
        let rows = store
            .list_v2_audit_events_for_runs_partitioned(
                "ws_a",
                &run_ids,
                Some("v2_envelope"),
                Some("step_recovery_attempted"),
                9,
            )
            .expect("partitioned list");
        let busy_rows = rows
            .iter()
            .filter(|row| row.run_id == busy)
            .collect::<Vec<_>>();
        let quiet_rows = rows
            .iter()
            .filter(|row| row.run_id == quiet)
            .collect::<Vec<_>>();
        assert_eq!(busy_rows.len(), 9);
        assert_eq!(quiet_rows.len(), 1);
        assert_eq!(quiet_rows[0].event_id, "evt-quiet-1");
        assert!(!rows.iter().any(|row| row.run_id == empty));

        let present = store
            .list_v2_audit_run_ids_with_events("ws_a", &run_ids, Some("v2_envelope"))
            .expect("presence");
        assert!(present.contains(busy));
        assert!(present.contains(quiet));
        assert!(!present.contains(empty));
    }

    fn insert_envelope(
        store: &Store,
        run_id: &str,
        event_id: &str,
        minute: u32,
        body_kind: Option<&str>,
    ) {
        let ts = chrono::TimeZone::with_ymd_and_hms(&Utc, 2026, 5, 1, 0, minute, 0)
            .single()
            .expect("ts");
        let payload = serde_json::json!({
            "event_id": event_id,
            "body_kind": body_kind,
            "run_id": run_id,
        });
        store
            .insert_v2_audit_event(&V2AuditEventInsertParams {
                workspace_id: "ws_a".to_string(),
                event_id: event_id.to_string(),
                source: "v2_envelope".to_string(),
                schema_version: 1,
                event_type: "test.event".to_string(),
                ts,
                run_id: run_id.to_string(),
                agent_identity: "codex".to_string(),
                parent_event_id: None,
                workspace_path: None,
                payload_json: payload.to_string(),
            })
            .expect("insert envelope");
    }
}
