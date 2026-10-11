use orbit_common::OrbitError;

use crate::contracts::{V2AuditEventFilter, V2AuditEventRow};

use super::rows::{collect_rows, row_to_v2_audit_event};

pub(super) fn run_ids_filter_sql(
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

pub(super) fn v2_filter_sql(
    filter: &V2AuditEventFilter,
) -> (String, Vec<Box<dyn rusqlite::types::ToSql>>) {
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

pub(super) fn count_matching(
    conn: &rusqlite::Connection,
    where_clause: &str,
    params: &[Box<dyn rusqlite::types::ToSql>],
) -> Result<i64, OrbitError> {
    let sql = format!("SELECT COUNT(*) FROM v2_audit_events {where_clause}");
    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|b| b.as_ref()).collect();
    conn.query_row(&sql, param_refs.as_slice(), |row| row.get(0))
        .map_err(|e| OrbitError::Store(e.to_string()))
}

pub(super) fn select_tail_page(
    conn: &rusqlite::Connection,
    where_clause: &str,
    mut params: Vec<Box<dyn rusqlite::types::ToSql>>,
    limit: usize,
    offset: usize,
) -> Result<Vec<V2AuditEventRow>, OrbitError> {
    let sql = format!(
        "SELECT id, workspace_id, event_id, source, schema_version, event_type, ts, \
         run_id, agent_identity, parent_event_id, workspace_path, payload_json \
         FROM v2_audit_events {where_clause} ORDER BY ts DESC, id DESC \
         LIMIT ?{} OFFSET ?{}",
        params.len() + 1,
        params.len() + 2
    );
    // Saturate: a wrapped negative LIMIT/OFFSET means "no bound" to SQLite.
    params.push(Box::new(i64::try_from(limit).unwrap_or(i64::MAX)));
    params.push(Box::new(i64::try_from(offset).unwrap_or(i64::MAX)));
    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|b| b.as_ref()).collect();
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| OrbitError::Store(e.to_string()))?;
    let rows = stmt
        .query_map(param_refs.as_slice(), row_to_v2_audit_event)
        .map_err(|e| OrbitError::Store(e.to_string()))?;
    collect_rows(rows)
}
