//! SQLite query adapter for the implementation-free failure-incident contract.

use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use orbit_types::telemetry::{AuditEvent, AuditEventStatus};

use crate::Store;
pub use crate::contracts::incident::*;

use super::{AUDIT_EVENT_COLUMNS, audit_event_from_row};

impl Store {
    pub fn get_failure_incidents(
        &self,
        query: &FailureIncidentQuery,
    ) -> Result<FailureIncidentReport, OrbitError> {
        let conn = self.read()?;
        let (sql, param_values) = failure_incident_scan_sql(query);
        let mut stmt = conn.prepare(&sql).map_err(sqlite_store_error)?;
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            param_values.iter().map(|value| value.as_ref()).collect();
        let failures = stmt
            .query_map(param_refs.as_slice(), audit_event_from_row)
            .map_err(sqlite_store_error)?
            .collect::<Result<Vec<AuditEvent>, _>>()
            .map_err(sqlite_store_error)?;
        let max_events = scan_limit(query);
        let truncated = failures.len() >= max_events;
        Ok(build_report(&failures, truncated))
    }
}

fn scan_limit(query: &FailureIncidentQuery) -> usize {
    if query.max_events == 0 {
        DEFAULT_SCAN_LIMIT
    } else {
        query.max_events
    }
}

/// Rows the incident grouping reads, newest id first, capped at the scan limit.
///
/// `status != 'success'` cannot seek `idx_audit_events_status`, and together
/// with `ORDER BY id DESC LIMIT` SQLite scans every audit row — successes
/// included. Equality on the non-success spellings seeks that index and reads
/// only failed rows. [`AuditEventStatus`] has no other non-success variant; a
/// new one has to join this list or the scan will omit it.
fn failure_incident_scan_sql(
    query: &FailureIncidentQuery,
) -> (String, Vec<Box<dyn rusqlite::types::ToSql>>) {
    let mut conditions = vec![non_success_status_predicate()];
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
    param_values.push(Box::new(scan_limit(query) as i64));
    (sql, param_values)
}

/// `status IN (...)` over every [`AuditEventStatus`] other than success.
///
/// Labels come from [`non_success_status_label`], whose match is exhaustive.
/// A new variant also has to be named in this loop or the scan will omit it.
fn non_success_status_predicate() -> String {
    let mut labels = Vec::new();
    for status in [
        AuditEventStatus::Success,
        AuditEventStatus::Failure,
        AuditEventStatus::Denied,
    ] {
        if let Some(label) = non_success_status_label(status) {
            labels.push(format!("'{label}'"));
        }
    }
    format!("status IN ({})", labels.join(", "))
}

/// Stored spelling of a non-success status, or `None` for success.
///
/// Exhaustive: a new [`AuditEventStatus`] variant fails compilation here until
/// the incident scan either reads it or leaves it out on purpose.
fn non_success_status_label(status: AuditEventStatus) -> Option<&'static str> {
    match status {
        AuditEventStatus::Success => None,
        AuditEventStatus::Failure => Some("failure"),
        AuditEventStatus::Denied => Some("denied"),
    }
}
