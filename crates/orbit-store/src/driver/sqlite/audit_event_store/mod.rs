//! Audit-event SQL queries backing the `orbit audit list` CLI.
//!
//! L-0009: callers should reach audit data via `orbit audit list --json` —
//! `<workspace>/.orbit/orbit.db` (and -shm/-wal siblings) is an abandoned
//! leftover from pre-two-root binaries, not a mirror of the canonical global
//! `~/.orbit/orbit.db`. The CLI and runtime always use the global store.

use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::telemetry::{
    ANONYMOUS_ACTOR_LABEL, AuditAttribution, AuditEvent, AuditEventStatus,
    canonical_actor_for_role_label,
};
use orbit_types::tool::{McpCapability, McpTransport};
use rusqlite::params;

use crate::{Store, StoreTx, now_string, parse_timestamp};

pub mod incident;

/// Canonical `SELECT` column list for a full [`AuditEvent`] row, in the order
/// [`audit_event_from_row`] indexes. Shared by every query that hydrates whole
/// events so a schema column can only be added in one place.
pub(super) const AUDIT_EVENT_COLUMNS: &str = "id, execution_id, timestamp, command, subcommand, \
     tool_name, target_type, target_id, role, status, exit_code, duration_ms, \
     working_directory, arguments_json, stdout_truncated, stderr_truncated, \
     error_message, host, pid, session_id, workspace_id, caller_machine_id, \
     caller_host_id, process_machine_id, process_host_id, transport, \
     capabilities_json, origin_session_id, mcp_call_id, lease_id, task_id, \
     job_run_id, activity_id, step_index, trace_id, caller_ip, \
     self_reported_actor";

use crate::contracts::{
    AuditActorAggregate, AuditAttributionAggregate, AuditEventFilter, AuditEventInsertParams,
    AuditInvocationFields, AuditRoleAggregate, AuditToolAggregate, AuditToolCallCountsByRole,
    AuditToolCallCountsBySurfaceAndRole, AuditTopToolCall,
};

fn audit_event_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AuditEvent> {
    let ts_raw: String = row.get(2)?;
    let status_raw: String = row.get(9)?;
    let timestamp = parse_timestamp(&ts_raw)?;
    let status = status_raw
        .parse::<AuditEventStatus>()
        .map_err(|error| invalid_text(9, &status_raw, error))?;
    let transport_raw: Option<String> = row.get(25)?;
    let transport = transport_raw
        .as_deref()
        .map(str::parse::<McpTransport>)
        .transpose()
        .map_err(|error| invalid_text(25, transport_raw.as_deref().unwrap_or_default(), error))?;
    let capabilities_raw: Option<String> = row.get(26)?;
    let effective_capabilities = capabilities_raw
        .as_deref()
        .map(serde_json::from_str::<BTreeSet<McpCapability>>)
        .transpose()
        .map_err(|error| {
            invalid_text(
                26,
                capabilities_raw.as_deref().unwrap_or_default(),
                error.to_string(),
            )
        })?
        .unwrap_or_default();

    Ok(AuditEvent {
        id: row.get(0)?,
        execution_id: row.get(1)?,
        timestamp,
        command: row.get(3)?,
        subcommand: row.get(4)?,
        tool_name: row.get(5)?,
        target_type: row.get(6)?,
        target_id: row.get(7)?,
        role: row.get(8)?,
        status,
        exit_code: row.get(10)?,
        duration_ms: row.get(11)?,
        working_directory: row.get(12)?,
        arguments_json: row.get(13)?,
        stdout_truncated: row.get(14)?,
        stderr_truncated: row.get(15)?,
        error_message: row.get(16)?,
        host: row.get(17)?,
        pid: row.get(18)?,
        session_id: row.get(19)?,
        workspace_id: row.get(20)?,
        caller_machine_id: row.get(21)?,
        caller_host_id: row.get(22)?,
        process_machine_id: row.get(23)?,
        process_host_id: row.get(24)?,
        transport,
        effective_capabilities,
        origin_session_id: row.get(27)?,
        mcp_call_id: row.get(28)?,
        trace_id: row.get(34)?,
        caller_ip: row.get(35)?,
        lease_id: row.get(29)?,
        task_id: row.get(30)?,
        job_run_id: row.get(31)?,
        activity_id: row.get(32)?,
        step_index: row.get(33)?,
        self_reported_actor: row.get(36)?,
    })
}

fn invalid_text(index: usize, raw: &str, error: impl Into<String>) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        index,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{} ({raw})", error.into()),
        )),
    )
}

impl Store {
    pub fn insert_audit_event_record(
        &self,
        params: &AuditEventInsertParams,
    ) -> Result<(), OrbitError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| OrbitError::Store(format!("mutex poisoned: {e}")))?;

        insert_audit_event_record_on_connection(&conn, params, AuditInvocationFields::default())
    }

    /// Insert a canonical audit row with the transport-neutral invocation
    /// fields carried by a tool session. The legacy insert DTO remains stable
    /// for non-tool producers; new invocation producers use this focused seam.
    pub fn insert_audit_event_record_with_invocation(
        &self,
        params: &AuditEventInsertParams,
        invocation: AuditInvocationFields<'_>,
    ) -> Result<(), OrbitError> {
        let conn = self
            .conn
            .lock()
            .map_err(|e| OrbitError::Store(format!("mutex poisoned: {e}")))?;

        insert_audit_event_record_on_connection(&conn, params, invocation)
    }
}

impl StoreTx<'_> {
    /// Insert one canonical audit row inside the caller's existing Store
    /// transaction. Vertical features use this when their domain mutation and
    /// its audit outcome must commit or roll back together.
    pub fn insert_audit_event_record(
        &mut self,
        params: &AuditEventInsertParams,
    ) -> Result<(), OrbitError> {
        insert_audit_event_record_on_connection(
            self.connection(),
            params,
            AuditInvocationFields::default(),
        )
    }
}

fn insert_audit_event_record_on_connection(
    conn: &rusqlite::Connection,
    params: &AuditEventInsertParams,
    invocation: AuditInvocationFields<'_>,
) -> Result<(), OrbitError> {
    let capabilities_json = serde_json::to_string(&params.effective_capabilities)
        .map_err(|error| OrbitError::Store(format!("serialize MCP capability set: {error}")))?;
    // ORB-10888: the canonical actor is derived from the same label the row
    // stores, by the same alias map the backfill uses, so new rows and
    // migrated rows land in identical aggregate buckets.
    //
    // ORB-10890: `invocation.self_reported_actor` is deliberately NOT an input
    // here. The trusted projection is a function of `role` alone, so a caller's
    // claim cannot reach any column an authorization or trust decision reads.
    let actor = canonical_actor_for_role_label(&params.role);

    conn.execute(
        r#"INSERT INTO audit_events(
            execution_id, timestamp, command, subcommand, tool_name,
            target_type, target_id, role, status, exit_code,
            duration_ms, working_directory, arguments_json,
            stdout_truncated, stderr_truncated, error_message,
            host, pid, session_id, workspace_id, caller_machine_id,
            caller_host_id, process_machine_id, process_host_id, transport,
            capabilities_json, origin_session_id, mcp_call_id, lease_id,
            task_id, job_run_id, activity_id, step_index, trace_id, caller_ip,
            actor_kind, actor_id, actor_vendor, actor_family, actor_model,
            actor_alias_version, self_reported_actor
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30, ?31, ?32, ?33, ?34, ?35, ?36, ?37, ?38, ?39, ?40, ?41, ?42)"#,
        rusqlite::params![
            params.execution_id,
            now_string(),
            params.command,
            params.subcommand,
            params.tool_name,
            params.target_type,
            params.target_id,
            params.role,
            params.status.to_string(),
            params.exit_code,
            params.duration_ms,
            params.working_directory,
            params.arguments_json,
            params.stdout_truncated,
            params.stderr_truncated,
            params.error_message,
            params.host,
            params.pid,
            params.session_id,
            params.workspace_id,
            params.caller_machine_id,
            params.caller_host_id,
            params.process_machine_id,
            params.process_host_id,
            params.transport.map(|transport| transport.to_string()),
            capabilities_json,
            params.origin_session_id,
            params.mcp_call_id,
            params.lease_id,
            params.task_id,
            params.job_run_id,
            params.activity_id,
            params.step_index,
            invocation.trace_id,
            invocation.caller_ip,
            actor.kind.as_str(),
            actor.id,
            actor.vendor,
            actor.family,
            actor.model,
            actor.alias_version,
            invocation.self_reported_actor,
        ],
    )
        .map_err(|e| OrbitError::Store(e.to_string()))?;

    Ok(())
}

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

    pub fn get_audit_event_stats(
        &self,
        since: Option<&DateTime<Utc>>,
        tool: Option<&str>,
    ) -> Result<(i64, i64, i64, i64, f64, i64), OrbitError> {
        let conn = self.read()?;

        let mut conditions = Vec::new();
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(since) = since {
            conditions.push(format!("timestamp >= ?{}", param_values.len() + 1));
            param_values.push(Box::new(since.to_rfc3339()));
        }
        if let Some(tool) = tool {
            conditions.push(format!("tool_name = ?{}", param_values.len() + 1));
            param_values.push(Box::new(tool.to_string()));
        }

        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conditions.join(" AND "))
        };

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

        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            param_values.iter().map(|b| b.as_ref()).collect();

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
        .map_err(|e| OrbitError::Store(e.to_string()))
    }

    pub fn get_audit_event_durations(
        &self,
        since: Option<&DateTime<Utc>>,
        tool: Option<&str>,
    ) -> Result<Vec<i64>, OrbitError> {
        let conn = self.read()?;

        let mut conditions = Vec::new();
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(since) = since {
            conditions.push(format!("timestamp >= ?{}", param_values.len() + 1));
            param_values.push(Box::new(since.to_rfc3339()));
        }
        if let Some(tool) = tool {
            conditions.push(format!("tool_name = ?{}", param_values.len() + 1));
            param_values.push(Box::new(tool.to_string()));
        }

        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conditions.join(" AND "))
        };

        let sql =
            format!("SELECT duration_ms FROM audit_events {where_clause} ORDER BY duration_ms ASC");

        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            param_values.iter().map(|b| b.as_ref()).collect();

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| OrbitError::Store(e.to_string()))?;

        let rows = stmt
            .query_map(param_refs.as_slice(), |row| row.get::<_, i64>(0))
            .map_err(|e| OrbitError::Store(e.to_string()))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| OrbitError::Store(e.to_string()))
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

        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| OrbitError::Store(e.to_string()))?;

        let rows = stmt
            .query_map(params![since.to_rfc3339()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(|e| OrbitError::Store(e.to_string()))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| OrbitError::Store(e.to_string()))
    }

    /// Returns `(role, denied_count)` for audit events with status='denied' and
    /// `timestamp >= since`, ordered desc by count. Used to join SQLite-level
    /// CLI denials onto the per-agent scoreboard.
    pub fn get_audit_denials_by_role(
        &self,
        since: Option<&DateTime<Utc>>,
    ) -> Result<Vec<(String, i64)>, OrbitError> {
        let conn = self.read()?;

        let sql = if since.is_some() {
            "SELECT role, COUNT(*) FROM audit_events \
             WHERE status = 'denied' AND timestamp >= ?1 \
             GROUP BY role ORDER BY COUNT(*) DESC"
        } else {
            "SELECT role, COUNT(*) FROM audit_events \
             WHERE status = 'denied' \
             GROUP BY role ORDER BY COUNT(*) DESC"
        };

        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| OrbitError::Store(e.to_string()))?;

        let rows = if let Some(s) = since {
            stmt.query_map(params![s.to_rfc3339()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(|e| OrbitError::Store(e.to_string()))?
            .collect::<Result<Vec<_>, _>>()
        } else {
            stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(|e| OrbitError::Store(e.to_string()))?
            .collect::<Result<Vec<_>, _>>()
        };

        rows.map_err(|e| OrbitError::Store(e.to_string()))
    }

    /// Returns `(operation, denied_count)` for `command = 'authorization'`
    /// audit events with `status = 'denied'` and `timestamp >= since`,
    /// ordered desc by count.
    ///
    /// Restricted to `command = 'authorization'` because that is the one row
    /// every denial chokepoint writes exactly once per refusal
    /// (`OrbitRuntime::record_authorization_event`, see its module doc); a
    /// tool-surface denial also gets a second, entry-point row from
    /// `execute_tool_dispatch_with_audit_store`, and counting both would
    /// double every tool-surface operation's total. `target_id` is that row's
    /// operation ID for every surface — unlike `tool_name`, which the
    /// authorization row deliberately leaves unset for `Tool`-surface
    /// operations to avoid colliding with the entry-point row's own
    /// `tool_name`.
    pub fn get_audit_denials_by_operation(
        &self,
        since: Option<&DateTime<Utc>>,
    ) -> Result<Vec<(String, i64)>, OrbitError> {
        let conn = self.read()?;

        let sql = if since.is_some() {
            "SELECT COALESCE(target_id, 'unknown'), COUNT(*) FROM audit_events \
             WHERE command = 'authorization' AND status = 'denied' AND timestamp >= ?1 \
             GROUP BY COALESCE(target_id, 'unknown') ORDER BY COUNT(*) DESC"
        } else {
            "SELECT COALESCE(target_id, 'unknown'), COUNT(*) FROM audit_events \
             WHERE command = 'authorization' AND status = 'denied' \
             GROUP BY COALESCE(target_id, 'unknown') ORDER BY COUNT(*) DESC"
        };

        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| OrbitError::Store(e.to_string()))?;

        let rows = if let Some(s) = since {
            stmt.query_map(params![s.to_rfc3339()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(|e| OrbitError::Store(e.to_string()))?
            .collect::<Result<Vec<_>, _>>()
        } else {
            stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(|e| OrbitError::Store(e.to_string()))?
            .collect::<Result<Vec<_>, _>>()
        };

        rows.map_err(|e| OrbitError::Store(e.to_string()))
    }

    pub fn get_audit_tool_call_counts_by_role(
        &self,
        since: Option<&DateTime<Utc>>,
    ) -> Result<Vec<AuditToolCallCountsByRole>, OrbitError> {
        let conn = self.read()?;

        let sql = if since.is_some() {
            "SELECT role, COUNT(*), \
             COALESCE(SUM(CASE WHEN status != 'success' THEN 1 ELSE 0 END), 0) \
             FROM audit_events \
             WHERE command = 'tool' \
               AND subcommand IN ('run', 'run-mcp') \
               AND tool_name IS NOT NULL \
               AND timestamp >= ?1 \
             GROUP BY role ORDER BY COUNT(*) DESC, role ASC"
        } else {
            "SELECT role, COUNT(*), \
             COALESCE(SUM(CASE WHEN status != 'success' THEN 1 ELSE 0 END), 0) \
             FROM audit_events \
             WHERE command = 'tool' \
               AND subcommand IN ('run', 'run-mcp') \
               AND tool_name IS NOT NULL \
             GROUP BY role ORDER BY COUNT(*) DESC, role ASC"
        };

        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| OrbitError::Store(e.to_string()))?;

        let rows = if let Some(s) = since {
            stmt.query_map(params![s.to_rfc3339()], |row| {
                Ok(AuditToolCallCountsByRole {
                    role: row.get(0)?,
                    total: row.get::<_, i64>(1)? as u64,
                    failed: row.get::<_, i64>(2)? as u64,
                })
            })
            .map_err(|e| OrbitError::Store(e.to_string()))?
            .collect::<Result<Vec<_>, _>>()
        } else {
            stmt.query_map([], |row| {
                Ok(AuditToolCallCountsByRole {
                    role: row.get(0)?,
                    total: row.get::<_, i64>(1)? as u64,
                    failed: row.get::<_, i64>(2)? as u64,
                })
            })
            .map_err(|e| OrbitError::Store(e.to_string()))?
            .collect::<Result<Vec<_>, _>>()
        };

        rows.map_err(|e| OrbitError::Store(e.to_string()))
    }

    /// Per-(actor, attribution) counts for audited tool invocations, over the
    /// same rows as [`Self::get_audit_tool_call_counts_by_role`] [ORB-10890].
    ///
    /// Each row is classified once, into exactly one of three disjoint
    /// buckets, so the three denominators the caller needs are all readable
    /// off one result set: filter on `attribution` for authenticated-only or
    /// self-reported-only, sum for combined.
    ///
    /// - **authenticated** — the ORB-10888 actor projection resolved to a real
    ///   caller (`actor_kind` present and not `unattributed`). Grouped on
    ///   `actor_id`, so one agent recorded at family, shorthand, and
    ///   full-model granularity is a single row.
    /// - **self_reported** — Orbit could not authenticate the caller, but the
    ///   caller named itself. Grouped on that claim, which is why the bucket
    ///   is kept separate: the label is only as good as the client's honesty.
    /// - **anonymous** — neither. This is the residue that motivated the
    ///   feature, and it stays visible instead of being absorbed.
    ///
    /// A NULL `actor_kind` (a row the v16 backfill could not reach) reads as
    /// unattributed, matching [`Self::get_audit_event_aggregates_by_actor`],
    /// so it falls through to the self-reported or anonymous bucket rather
    /// than being counted as authenticated.
    pub fn get_audit_tool_call_counts_by_attribution(
        &self,
        since: Option<&DateTime<Utc>>,
    ) -> Result<Vec<AuditAttributionAggregate>, OrbitError> {
        let conn = self.read()?;

        let authenticated = "actor_kind IS NOT NULL AND actor_kind != 'unattributed'";
        let attribution = format!(
            "CASE WHEN {authenticated} THEN 'authenticated' \
             WHEN self_reported_actor IS NOT NULL THEN 'self_reported' \
             ELSE 'anonymous' END"
        );
        let actor = format!(
            "CASE WHEN {authenticated} THEN actor_id \
             WHEN self_reported_actor IS NOT NULL THEN self_reported_actor \
             ELSE '{ANONYMOUS_ACTOR_LABEL}' END"
        );
        let window = if since.is_some() {
            "AND timestamp >= ?1 "
        } else {
            ""
        };
        let sql = format!(
            "SELECT {attribution} AS attribution, {actor} AS actor, COUNT(*), \
             COALESCE(SUM(CASE WHEN status != 'success' THEN 1 ELSE 0 END), 0), \
             COALESCE(SUM(CASE WHEN subcommand = 'run-mcp' THEN 1 ELSE 0 END), 0), \
             COALESCE(SUM(CASE WHEN subcommand = 'run' THEN 1 ELSE 0 END), 0) \
             FROM audit_events \
             WHERE command = 'tool' \
               AND subcommand IN ('run', 'run-mcp') \
               AND tool_name IS NOT NULL \
               {window}\
             GROUP BY attribution, actor \
             ORDER BY COUNT(*) DESC, attribution ASC, actor ASC"
        );

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| OrbitError::Store(e.to_string()))?;

        let map_row = |row: &rusqlite::Row<'_>| {
            let attribution_raw: String = row.get(0)?;
            let attribution = attribution_raw
                .parse::<AuditAttribution>()
                .map_err(|error| invalid_text(0, &attribution_raw, error))?;
            Ok(AuditAttributionAggregate {
                attribution,
                actor: row.get(1)?,
                total: row.get::<_, i64>(2)? as u64,
                failed: row.get::<_, i64>(3)? as u64,
                mcp: row.get::<_, i64>(4)? as u64,
                cli: row.get::<_, i64>(5)? as u64,
            })
        };

        let rows = if let Some(s) = since {
            stmt.query_map(params![s.to_rfc3339()], map_row)
                .map_err(|e| OrbitError::Store(e.to_string()))?
                .collect::<Result<Vec<_>, _>>()
        } else {
            stmt.query_map([], map_row)
                .map_err(|e| OrbitError::Store(e.to_string()))?
                .collect::<Result<Vec<_>, _>>()
        };

        rows.map_err(|e| OrbitError::Store(e.to_string()))
    }

    /// Per-(surface, role) tool call counts where `tool_name` matches
    /// `orbit.<surface>.<verb>`. The surface segment is extracted with
    /// SQLite string functions so we don't need a regex extension.
    /// `failed` counts every non-`success` row (failure + denied) like
    /// [`Self::get_audit_tool_call_counts_by_role`].
    pub fn get_audit_tool_call_counts_by_surface_and_role(
        &self,
        since: Option<&DateTime<Utc>>,
    ) -> Result<Vec<AuditToolCallCountsBySurfaceAndRole>, OrbitError> {
        let conn = self.read()?;

        // SUBSTR(tool_name, 7) strips the literal "orbit." prefix; the
        // appended "." in the inner SUBSTR ensures INSTR finds a delimiter
        // even for names with no third segment (e.g. "orbit.task" → surface
        // "task"). The outer LIKE filter discards anything that does not
        // start with "orbit." entirely.
        let extract = "SUBSTR(tool_name, 7, INSTR(SUBSTR(tool_name, 7) || '.', '.') - 1)";
        let sql = if since.is_some() {
            format!(
                "SELECT {extract} AS surface, role, COUNT(*), \
                 COALESCE(SUM(CASE WHEN status != 'success' THEN 1 ELSE 0 END), 0) \
                 FROM audit_events \
                 WHERE command = 'tool' \
                   AND subcommand IN ('run', 'run-mcp') \
                   AND tool_name LIKE 'orbit.%' \
                   AND timestamp >= ?1 \
                 GROUP BY surface, role \
                 ORDER BY surface ASC, COUNT(*) DESC, role ASC"
            )
        } else {
            format!(
                "SELECT {extract} AS surface, role, COUNT(*), \
                 COALESCE(SUM(CASE WHEN status != 'success' THEN 1 ELSE 0 END), 0) \
                 FROM audit_events \
                 WHERE command = 'tool' \
                   AND subcommand IN ('run', 'run-mcp') \
                   AND tool_name LIKE 'orbit.%' \
                 GROUP BY surface, role \
                 ORDER BY surface ASC, COUNT(*) DESC, role ASC"
            )
        };

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| OrbitError::Store(e.to_string()))?;

        let rows = if let Some(s) = since {
            stmt.query_map(params![s.to_rfc3339()], |row| {
                Ok(AuditToolCallCountsBySurfaceAndRole {
                    surface: row.get(0)?,
                    role: row.get(1)?,
                    total: row.get::<_, i64>(2)? as u64,
                    failed: row.get::<_, i64>(3)? as u64,
                })
            })
            .map_err(|e| OrbitError::Store(e.to_string()))?
            .collect::<Result<Vec<_>, _>>()
        } else {
            stmt.query_map([], |row| {
                Ok(AuditToolCallCountsBySurfaceAndRole {
                    surface: row.get(0)?,
                    role: row.get(1)?,
                    total: row.get::<_, i64>(2)? as u64,
                    failed: row.get::<_, i64>(3)? as u64,
                })
            })
            .map_err(|e| OrbitError::Store(e.to_string()))?
            .collect::<Result<Vec<_>, _>>()
        };

        rows.map_err(|e| OrbitError::Store(e.to_string()))
    }

    /// Top (role, tool_name) pairs by call count across the audit log,
    /// limited to `orbit.*` tool names. The optional `since` filter, when
    /// supplied, scopes the query to events at-or-after that timestamp.
    /// `limit` caps the row count after sorting; `0` means no cap.
    ///
    /// Sort key: total DESC, then tool_name ASC, then role ASC for stable
    /// output across runs.
    pub fn get_audit_top_tool_calls(
        &self,
        since: Option<&DateTime<Utc>>,
        limit: usize,
    ) -> Result<Vec<AuditTopToolCall>, OrbitError> {
        let conn = self.read()?;

        let base = "SELECT tool_name, role, COUNT(*) \
                    FROM audit_events \
                    WHERE command = 'tool' \
                      AND subcommand IN ('run', 'run-mcp') \
                      AND tool_name LIKE 'orbit.%'";
        let order = "GROUP BY tool_name, role \
                     ORDER BY COUNT(*) DESC, tool_name ASC, role ASC";
        let sql = match (since.is_some(), limit > 0) {
            (true, true) => format!("{base} AND timestamp >= ?1 {order} LIMIT ?2"),
            (true, false) => format!("{base} AND timestamp >= ?1 {order}"),
            (false, true) => format!("{base} {order} LIMIT ?1"),
            (false, false) => format!("{base} {order}"),
        };

        let mut stmt = conn
            .prepare(&sql)
            .map_err(|e| OrbitError::Store(e.to_string()))?;

        let map_row = |row: &rusqlite::Row<'_>| {
            Ok(AuditTopToolCall {
                tool_name: row.get(0)?,
                role: row.get(1)?,
                total: row.get::<_, i64>(2)? as u64,
            })
        };

        let rows = match (since, limit) {
            (Some(s), 0) => stmt
                .query_map(params![s.to_rfc3339()], map_row)
                .map_err(|e| OrbitError::Store(e.to_string()))?
                .collect::<Result<Vec<_>, _>>(),
            (Some(s), n) => stmt
                .query_map(params![s.to_rfc3339(), n as i64], map_row)
                .map_err(|e| OrbitError::Store(e.to_string()))?
                .collect::<Result<Vec<_>, _>>(),
            (None, 0) => stmt
                .query_map([], map_row)
                .map_err(|e| OrbitError::Store(e.to_string()))?
                .collect::<Result<Vec<_>, _>>(),
            (None, n) => stmt
                .query_map(params![n as i64], map_row)
                .map_err(|e| OrbitError::Store(e.to_string()))?
                .collect::<Result<Vec<_>, _>>(),
        };

        rows.map_err(|e| OrbitError::Store(e.to_string()))
    }

    /// Sorted `duration_ms` values for audit events with NULL `tool_name`
    /// at or after `since`. Mirror of [`Self::get_audit_event_durations`]
    /// for the synthetic `"unknown"` bucket that
    /// [`Self::get_audit_event_aggregates_by_tool`] surfaces — that aggregate
    /// folds NULL tool names into `"unknown"` for counts, and this method
    /// lets the caller compute the same bucket's percentiles.
    pub fn get_audit_event_durations_null_tool(
        &self,
        since: &DateTime<Utc>,
    ) -> Result<Vec<i64>, OrbitError> {
        let conn = self.read()?;

        let sql = "SELECT duration_ms FROM audit_events \
                   WHERE tool_name IS NULL AND timestamp >= ?1 \
                   ORDER BY duration_ms ASC";

        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| OrbitError::Store(e.to_string()))?;

        let rows = stmt
            .query_map(params![since.to_rfc3339()], |row| row.get::<_, i64>(0))
            .map_err(|e| OrbitError::Store(e.to_string()))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| OrbitError::Store(e.to_string()))
    }

    /// Per-tool aggregate of audit events with `timestamp >= since`. Folds
    /// NULL `tool_name` into a synthetic `"unknown"` bucket so callers don't
    /// have to guard against missing values. The `mcp_*` / `cli_*` columns
    /// only count rows where `subcommand` is `'run-mcp'` or `'run'` respectively;
    /// other subcommands contribute to `total` and `failures` but not to the
    /// split.
    pub fn get_audit_event_aggregates_by_tool(
        &self,
        since: &DateTime<Utc>,
    ) -> Result<Vec<AuditToolAggregate>, OrbitError> {
        let conn = self.read()?;

        let sql = "SELECT COALESCE(tool_name, 'unknown') AS tool, \
                   COUNT(*), \
                   COALESCE(SUM(CASE WHEN status = 'success' THEN 1 ELSE 0 END), 0), \
                   COALESCE(SUM(CASE WHEN status = 'failure' THEN 1 ELSE 0 END), 0), \
                   COALESCE(SUM(CASE WHEN status = 'denied' THEN 1 ELSE 0 END), 0), \
                   COALESCE(SUM(CASE WHEN subcommand = 'run-mcp' THEN 1 ELSE 0 END), 0), \
                   COALESCE(SUM(CASE WHEN subcommand = 'run' THEN 1 ELSE 0 END), 0), \
                   COALESCE(SUM(CASE WHEN status = 'failure' AND subcommand = 'run-mcp' THEN 1 ELSE 0 END), 0), \
                   COALESCE(SUM(CASE WHEN status = 'failure' AND subcommand = 'run' THEN 1 ELSE 0 END), 0), \
                   COALESCE(AVG(duration_ms), 0.0) \
                   FROM audit_events WHERE timestamp >= ?1 GROUP BY tool";

        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| OrbitError::Store(e.to_string()))?;

        let rows = stmt
            .query_map(params![since.to_rfc3339()], |row| {
                Ok(AuditToolAggregate {
                    tool_name: row.get(0)?,
                    total: row.get(1)?,
                    successes: row.get(2)?,
                    failures: row.get(3)?,
                    denials: row.get(4)?,
                    mcp_total: row.get(5)?,
                    cli_total: row.get(6)?,
                    mcp_failures: row.get(7)?,
                    cli_failures: row.get(8)?,
                    avg_duration_ms: row.get(9)?,
                })
            })
            .map_err(|e| OrbitError::Store(e.to_string()))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| OrbitError::Store(e.to_string()))
    }

    /// Per-role aggregate of audit events with `timestamp >= since`, including
    /// the MCP-vs-CLI surface split. Every row where `subcommand` is neither
    /// `'run'` nor `'run-mcp'` is still counted toward `total`, and lands in
    /// exactly one of `other` (a different non-null subcommand) or
    /// `no_subcommand` (subcommand is `NULL`, e.g. internal lock traffic) —
    /// so `mcp + cli + other + no_subcommand == total` for every row
    /// (ORB-10889). `other` uses `subcommand IS NOT NULL AND ... NOT IN`
    /// rather than a bare `NOT IN`, since SQL's `NULL NOT IN (...)` evaluates
    /// to `NULL`/false and would silently drop the `no_subcommand` bucket.
    pub fn get_audit_event_aggregates_by_role(
        &self,
        since: &DateTime<Utc>,
    ) -> Result<Vec<AuditRoleAggregate>, OrbitError> {
        let conn = self.read()?;

        let sql = "SELECT role, \
                   COUNT(*), \
                   COALESCE(SUM(CASE WHEN subcommand = 'run-mcp' THEN 1 ELSE 0 END), 0), \
                   COALESCE(SUM(CASE WHEN subcommand = 'run' THEN 1 ELSE 0 END), 0), \
                   COALESCE(SUM(CASE WHEN subcommand IS NOT NULL AND subcommand NOT IN ('run-mcp', 'run') THEN 1 ELSE 0 END), 0), \
                   COALESCE(SUM(CASE WHEN subcommand IS NULL THEN 1 ELSE 0 END), 0) \
                   FROM audit_events WHERE timestamp >= ?1 \
                   GROUP BY role ORDER BY COUNT(*) DESC, role ASC";

        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| OrbitError::Store(e.to_string()))?;

        let rows = stmt
            .query_map(params![since.to_rfc3339()], |row| {
                Ok(AuditRoleAggregate {
                    role: row.get(0)?,
                    total: row.get(1)?,
                    mcp: row.get(2)?,
                    cli: row.get(3)?,
                    other: row.get(4)?,
                    no_subcommand: row.get(5)?,
                })
            })
            .map_err(|e| OrbitError::Store(e.to_string()))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| OrbitError::Store(e.to_string()))
    }

    /// Per-canonical-actor aggregate of audit events with `timestamp >= since`,
    /// carrying the same MCP-vs-CLI split as
    /// [`Self::get_audit_event_aggregates_by_role`] (ORB-10888).
    ///
    /// Groups on the materialized actor projection rather than the raw `role`
    /// label, so one agent recorded at family, shorthand, and full-model
    /// granularity aggregates as one row and synthetic buckets are separable by
    /// `kind` without string-matching the label. Rows written before the
    /// projection existed are backfilled by migration v16, so old and new rows
    /// group together.
    pub fn get_audit_event_aggregates_by_actor(
        &self,
        since: &DateTime<Utc>,
    ) -> Result<Vec<AuditActorAggregate>, OrbitError> {
        let conn = self.read()?;

        // COALESCE guards a row the backfill could not reach (a database
        // opened read-only mid-upgrade); it reports as unattributed rather
        // than collapsing every such row into one NULL bucket.
        let sql = "SELECT COALESCE(actor_kind, 'unattributed'), \
                   COALESCE(actor_id, 'unknown'), \
                   actor_vendor, \
                   actor_family, \
                   COUNT(*), \
                   COALESCE(SUM(CASE WHEN subcommand = 'run-mcp' THEN 1 ELSE 0 END), 0), \
                   COALESCE(SUM(CASE WHEN subcommand = 'run' THEN 1 ELSE 0 END), 0) \
                   FROM audit_events WHERE timestamp >= ?1 \
                   GROUP BY 1, 2, 3, 4 \
                   ORDER BY COUNT(*) DESC, 1 ASC, 2 ASC";

        let mut stmt = conn
            .prepare(sql)
            .map_err(|e| OrbitError::Store(e.to_string()))?;

        let rows = stmt
            .query_map(params![since.to_rfc3339()], |row| {
                Ok(AuditActorAggregate {
                    kind: row.get(0)?,
                    actor: row.get(1)?,
                    vendor: row.get(2)?,
                    family: row.get(3)?,
                    total: row.get(4)?,
                    mcp: row.get(5)?,
                    cli: row.get(6)?,
                })
            })
            .map_err(|e| OrbitError::Store(e.to_string()))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| OrbitError::Store(e.to_string()))
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

use rusqlite::OptionalExtension;

#[cfg(test)]
#[cfg(test)]
mod tests;
