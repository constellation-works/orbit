//! Audit-event aggregates: denials, tool-call counts, and per-tool, per-role
//! and per-actor rollups.

use chrono::{DateTime, Utc};
use orbit_common::OrbitError;
use orbit_types::telemetry::{ANONYMOUS_ACTOR_LABEL, AuditAttribution};
use rusqlite::params;

use crate::Store;
use crate::contracts::{
    AuditActorAggregate, AuditAttributionAggregate, AuditRoleAggregate, AuditToolAggregate,
    AuditToolCallCountsByRole, AuditToolCallCountsBySurfaceAndRole, AuditTopToolCall,
};

use super::row::invalid_text;

impl Store {
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
}
