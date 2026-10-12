//! SQLite query adapter for the implementation-free failure-incident contract.

use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use orbit_types::telemetry::AuditEvent;

use crate::Store;
pub use crate::contracts::incident::*;

use super::{AUDIT_EVENT_COLUMNS, audit_event_from_row};

impl Store {
    pub fn get_failure_incidents(
        &self,
        query: &FailureIncidentQuery,
    ) -> Result<FailureIncidentReport, OrbitError> {
        let max_events = if query.max_events == 0 {
            DEFAULT_SCAN_LIMIT
        } else {
            query.max_events
        };
        let conn = self.read()?;
        let mut conditions = vec!["status != 'success'".to_string()];
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        if let Some(since) = query.since {
            conditions.push(format!("timestamp >= ?{}", param_values.len() + 1));
            param_values.push(Box::new(since.to_rfc3339()));
        }
        if let Some(until) = query.until {
            conditions.push(format!("timestamp <= ?{}", param_values.len() + 1));
            param_values.push(Box::new(until.to_rfc3339()));
        }
        if let Some(role) = query.role.as_deref().filter(|role| !role.is_empty()) {
            conditions.push(format!("role = ?{}", param_values.len() + 1));
            param_values.push(Box::new(role.to_string()));
        }
        if let Some(workspace_id) = query
            .workspace_id
            .as_deref()
            .filter(|workspace_id| !workspace_id.is_empty())
        {
            conditions.push(format!("workspace_id = ?{}", param_values.len() + 1));
            param_values.push(Box::new(workspace_id.to_string()));
        }
        let sql = format!(
            "SELECT {AUDIT_EVENT_COLUMNS} FROM audit_events WHERE {} \
             ORDER BY id DESC LIMIT ?{}",
            conditions.join(" AND "),
            param_values.len() + 1
        );
        param_values.push(Box::new(max_events as i64));
        let mut stmt = conn.prepare(&sql).map_err(sqlite_store_error)?;
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            param_values.iter().map(|value| value.as_ref()).collect();
        let failures = stmt
            .query_map(param_refs.as_slice(), audit_event_from_row)
            .map_err(sqlite_store_error)?
            .collect::<Result<Vec<AuditEvent>, _>>()
            .map_err(sqlite_store_error)?;
        let truncated = failures.len() >= max_events;
        Ok(build_report(&failures, truncated))
    }
}
