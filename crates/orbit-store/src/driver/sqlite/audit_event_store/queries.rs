//! Audit-event reads by id or filter, and retention pruning.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::telemetry::AuditEvent;
use rusqlite::{OptionalExtension, params};

use crate::Store;
use crate::contracts::AuditEventFilter;

use super::row::{AUDIT_EVENT_COLUMNS, audit_event_from_row};

impl Store {
    pub fn list_audit_events(
        &self,
        filter: &AuditEventFilter,
    ) -> Result<Vec<AuditEvent>, OrbitError> {
        let conn = self.read()?;

        let mut conditions = Vec::new();
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(ref since) = filter.since {
            conditions.push(format!("timestamp >= ?{}", param_values.len() + 1));
            param_values.push(Box::new(since.to_rfc3339()));
        }
        if let Some(ref tool) = filter.tool_name {
            conditions.push(format!("tool_name = ?{}", param_values.len() + 1));
            param_values.push(Box::new(tool.clone()));
        }
        if let Some(ref target_type) = filter.target_type {
            conditions.push(format!("target_type = ?{}", param_values.len() + 1));
            param_values.push(Box::new(target_type.clone()));
        }
        if let Some(ref status) = filter.status {
            conditions.push(format!("status = ?{}", param_values.len() + 1));
            param_values.push(Box::new(status.to_string()));
        }
        if let Some(ref role) = filter.role {
            conditions.push(format!("role = ?{}", param_values.len() + 1));
            param_values.push(Box::new(role.clone()));
        }
        if let Some(ref workspace_id) = filter.workspace_id {
            conditions.push(format!("workspace_id = ?{}", param_values.len() + 1));
            param_values.push(Box::new(workspace_id.clone()));
        }
        if let Some(ref machine_id) = filter.caller_machine_id {
            conditions.push(format!("caller_machine_id = ?{}", param_values.len() + 1));
            param_values.push(Box::new(machine_id.clone()));
        }
        if let Some(ref machine_id) = filter.process_machine_id {
            conditions.push(format!("process_machine_id = ?{}", param_values.len() + 1));
            param_values.push(Box::new(machine_id.clone()));
        }
        if let Some(transport) = filter.transport {
            conditions.push(format!("transport = ?{}", param_values.len() + 1));
            param_values.push(Box::new(transport.to_string()));
        }
        if let Some(capability) = filter.capability {
            conditions.push(format!(
                "EXISTS (SELECT 1 FROM json_each(COALESCE(capabilities_json, '[]')) \
                 WHERE json_each.value = ?{})",
                param_values.len() + 1
            ));
            param_values.push(Box::new(capability.to_string()));
        }
        if let Some(ref origin_session_id) = filter.origin_session_id {
            conditions.push(format!("origin_session_id = ?{}", param_values.len() + 1));
            param_values.push(Box::new(origin_session_id.clone()));
        }
        if let Some(ref mcp_call_id) = filter.mcp_call_id {
            conditions.push(format!("mcp_call_id = ?{}", param_values.len() + 1));
            param_values.push(Box::new(mcp_call_id.clone()));
        }
        if let Some(ref job_run_id) = filter.job_run_id {
            conditions.push(format!("job_run_id = ?{}", param_values.len() + 1));
            param_values.push(Box::new(job_run_id.clone()));
        }
        if let Some(ref lease_id) = filter.lease_id {
            conditions.push(format!("lease_id = ?{}", param_values.len() + 1));
            param_values.push(Box::new(lease_id.clone()));
        }

        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conditions.join(" AND "))
        };

        let limit = if filter.limit == 0 {
            1000
        } else {
            filter.limit
        };

        let sql = format!(
            "SELECT {AUDIT_EVENT_COLUMNS} \
             FROM audit_events {where_clause} ORDER BY id DESC LIMIT ?{} OFFSET ?{}",
            param_values.len() + 1,
            param_values.len() + 2
        );

        param_values.push(Box::new(limit as i64));
        param_values.push(Box::new(filter.offset as i64));

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| OrbitError::Store(e.to_string()))?;

        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            param_values.iter().map(|b| b.as_ref()).collect();

        let rows = stmt
            .query_map(param_refs.as_slice(), audit_event_from_row)
            .map_err(|e| OrbitError::Store(e.to_string()))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| OrbitError::Store(e.to_string()))
    }

    pub fn get_audit_event(&self, id: i64) -> Result<Option<AuditEvent>, OrbitError> {
        let conn = self.read()?;

        let mut stmt = conn
            .prepare(&format!(
                "SELECT {AUDIT_EVENT_COLUMNS} FROM audit_events WHERE id = ?1"
            ))
            .map_err(|e| OrbitError::Store(e.to_string()))?;

        let result = stmt
            .query_row(params![id], audit_event_from_row)
            .optional()
            .map_err(|e| OrbitError::Store(e.to_string()))?;

        Ok(result)
    }

    pub fn prune_audit_events(&self, older_than: &DateTime<Utc>) -> Result<usize, OrbitError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| OrbitError::Store(format!("mutex poisoned: {e}")))?;

        let count = conn
            .execute(
                "DELETE FROM audit_events WHERE timestamp < ?1",
                params![older_than.to_rfc3339()],
            )
            .map_err(|e| OrbitError::Store(e.to_string()))?;

        Ok(count)
    }
}
