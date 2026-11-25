//! Audit-event aggregates: denials, tool-call attribution counts, and
//! per-tool, per-role and per-actor rollups.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;
use orbit_types::telemetry::{ANONYMOUS_ACTOR_LABEL, AuditAttribution};
use rusqlite::{Connection, params};

use crate::contracts::{
    AuditActorAggregate, AuditAttributionAggregate, AuditRoleAggregate, AuditToolAggregate,
    AuditToolModelAggregate,
};

use super::row::invalid_text;

/// Capability and policy decisions, counted once per invocation and operation.
///
/// Authorization rows are the canonical capability decisions. Legacy tool
/// wrappers repeat their message without a shared invocation ID, so those
/// wrappers are evidence only. Other denied tool rows are policy decisions,
/// except lock contention and owner-side claim-settlement and late-effort
/// protocol refusals.
/// Raw audit status counts and listings intentionally retain every row.
pub(super) fn denials_by_operation(
    conn: &Connection,
    since: Option<&DateTime<Utc>>,
) -> Result<Vec<(String, i64)>, OrbitError> {
    let sql = "WITH decisions AS (
        SELECT COALESCE(tool_name, target_id, 'unknown') AS operation,
               CASE
                 WHEN NULLIF(trace_id, '') IS NOT NULL THEN 'trace:' || trace_id
                 WHEN NULLIF(mcp_call_id, '') IS NOT NULL THEN 'mcp:' ||
                   json_array(caller_machine_id, origin_session_id, session_id, mcp_call_id)
                 WHEN NULLIF(execution_id, '') IS NOT NULL THEN 'exec:' || execution_id
                 ELSE 'row:' || id
               END AS attempt
        FROM audit_events
        WHERE status = 'denied' AND (?1 IS NULL OR timestamp >= ?1)
          AND (command = 'authorization' OR (
            command = 'tool'
            AND COALESCE(tool_name, '') NOT IN (
              'orbit.task.locks.reserve', 'orbit.workspace.claim.acquire',
              'orbit.workspace.claim.release', 'orbit.drain.claim.settle',
              'orbit.drain.claim.effort'
            )
            AND COALESCE(error_message, '') NOT GLOB
              'operation * requires the `*` capability (*)*'
          ))
    )
    SELECT operation, COUNT(DISTINCT attempt) FROM decisions
    GROUP BY operation ORDER BY COUNT(DISTINCT attempt) DESC, operation ASC";
    let mut stmt = conn.prepare(sql).map_err(sqlite_store_error)?;
    stmt.query_map(params![since.map(DateTime::to_rfc3339)], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })
    .map_err(sqlite_store_error)?
    .collect::<Result<Vec<_>, _>>()
    .map_err(sqlite_store_error)
}

/// Per-(actor, attribution) counts for audited tool invocations
/// (`command = 'tool'`, `run` or `run-mcp`, named tool) [ORB-10890].
pub(super) fn tool_call_counts_by_attribution(
    conn: &Connection,
    since: Option<&DateTime<Utc>>,
) -> Result<Vec<AuditAttributionAggregate>, OrbitError> {
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

    let mut stmt = conn.prepare(&sql).map_err(sqlite_store_error)?;

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
            .map_err(sqlite_store_error)?
            .collect::<Result<Vec<_>, _>>()
    } else {
        stmt.query_map([], map_row)
            .map_err(sqlite_store_error)?
            .collect::<Result<Vec<_>, _>>()
    };

    rows.map_err(sqlite_store_error)
}

/// Per-tool aggregate of audit events with `timestamp >= since`.
pub(super) fn aggregates_by_tool(
    conn: &Connection,
    since: &DateTime<Utc>,
) -> Result<Vec<AuditToolAggregate>, OrbitError> {
    let sql = "SELECT COALESCE(tool_name, 'unknown') AS tool, \
               COUNT(*), \
               COALESCE(SUM(CASE WHEN status = 'success' THEN 1 ELSE 0 END), 0), \
               COALESCE(SUM(CASE WHEN status = 'failure' THEN 1 ELSE 0 END), 0), \
               COALESCE(SUM(CASE WHEN status = 'denied' THEN 1 ELSE 0 END), 0), \
               COALESCE(SUM(CASE WHEN command = 'tool' AND subcommand = 'run-mcp' THEN 1 ELSE 0 END), 0), \
               COALESCE(SUM(CASE WHEN command = 'tool' AND subcommand = 'run' THEN 1 ELSE 0 END), 0), \
               COALESCE(SUM(CASE WHEN command = 'tool' AND status = 'failure' AND subcommand = 'run-mcp' THEN 1 ELSE 0 END), 0), \
               COALESCE(SUM(CASE WHEN command = 'tool' AND status = 'failure' AND subcommand = 'run' THEN 1 ELSE 0 END), 0), \
               COALESCE(AVG(duration_ms), 0.0), \
               COALESCE(SUM(CASE WHEN command = 'tool' AND status = 'denied' AND subcommand IN ('run', 'run-mcp') THEN 1 ELSE 0 END), 0), \
               COALESCE(SUM(CASE WHEN command = 'tool' AND status = 'failure' AND subcommand IN ('run', 'run-mcp') \
                 AND orbit_failure_class(COALESCE(tool_name, 'unknown'), status, error_message) = 'unexpected' THEN 1 ELSE 0 END), 0) \
               FROM audit_events WHERE timestamp >= ?1 GROUP BY tool";

    let mut stmt = conn.prepare(sql).map_err(sqlite_store_error)?;

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
                callable_denials: row.get(10)?,
                callable_unexpected_failures: row.get(11)?,
            })
        })
        .map_err(sqlite_store_error)?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_store_error)
}

/// Callable tool calls (`run` and `run-mcp`) at or after `since`, grouped
/// by tool and server-derived model.
pub(super) fn aggregates_by_tool_model(
    conn: &Connection,
    since: &DateTime<Utc>,
) -> Result<Vec<AuditToolModelAggregate>, OrbitError> {
    let sql = "SELECT COALESCE(NULLIF(tool_name, ''), 'unknown') AS tool, \
               COALESCE(NULLIF(session_model, ''), NULLIF(actor_model, ''), NULLIF(actor_family, ''), 'unknown') AS model, \
               COALESCE(SUM(CASE WHEN status = 'success' THEN 1 ELSE 0 END), 0), \
               COALESCE(SUM(CASE WHEN status = 'failure' THEN 1 ELSE 0 END), 0), \
               COALESCE(SUM(CASE WHEN status = 'denied' THEN 1 ELSE 0 END), 0), \
               COALESCE(SUM(CASE WHEN status = 'failure' \
                 AND orbit_failure_class(COALESCE(NULLIF(tool_name, ''), 'unknown'), status, error_message) = 'unexpected' THEN 1 ELSE 0 END), 0) \
               FROM audit_events \
               WHERE timestamp >= ?1 AND command = 'tool' AND subcommand IN ('run', 'run-mcp') \
               GROUP BY tool, model \
               ORDER BY model ASC, tool ASC";
    let mut stmt = conn.prepare(sql).map_err(sqlite_store_error)?;
    let rows = stmt
        .query_map(params![since.to_rfc3339()], |row| {
            Ok(AuditToolModelAggregate {
                tool_name: row.get(0)?,
                model: row.get(1)?,
                successes: row.get(2)?,
                failures: row.get(3)?,
                denials: row.get(4)?,
                unexpected_failures: row.get(5)?,
            })
        })
        .map_err(sqlite_store_error)?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_store_error)
}

/// Per-role aggregate of audit events with `timestamp >= since`.
pub(super) fn aggregates_by_role(
    conn: &Connection,
    since: &DateTime<Utc>,
) -> Result<Vec<AuditRoleAggregate>, OrbitError> {
    // `+role` keeps the planner off `idx_audit_events_role`: walking it to
    // skip the GROUP BY sort visits every row of the table, where the
    // timestamp index reads only the window.
    let sql = "SELECT role, \
               COUNT(*), \
               COALESCE(SUM(CASE WHEN subcommand = 'run-mcp' THEN 1 ELSE 0 END), 0), \
               COALESCE(SUM(CASE WHEN subcommand = 'run' THEN 1 ELSE 0 END), 0), \
               COALESCE(SUM(CASE WHEN subcommand IS NOT NULL AND subcommand NOT IN ('run-mcp', 'run') THEN 1 ELSE 0 END), 0), \
               COALESCE(SUM(CASE WHEN subcommand IS NULL THEN 1 ELSE 0 END), 0) \
               FROM audit_events WHERE timestamp >= ?1 \
               GROUP BY +role ORDER BY COUNT(*) DESC, role ASC";

    let mut stmt = conn.prepare(sql).map_err(sqlite_store_error)?;

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
        .map_err(sqlite_store_error)?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_store_error)
}

/// Per-canonical-actor aggregate of audit events with `timestamp >= since`.
pub(super) fn aggregates_by_actor(
    conn: &Connection,
    since: &DateTime<Utc>,
) -> Result<Vec<AuditActorAggregate>, OrbitError> {
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

    let mut stmt = conn.prepare(sql).map_err(sqlite_store_error)?;

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
        .map_err(sqlite_store_error)?;

    rows.collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_store_error)
}
