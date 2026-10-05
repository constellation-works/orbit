use orbit_common::OrbitError;
use orbit_types::telemetry::{ACTOR_ALIAS_MAP_VERSION, canonical_actor_for_role_label};
use rusqlite::Connection;

use super::introspect::{add_column_if_missing, table_exists, table_has_column};

pub(super) fn ensure_audit_events_schema(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        r#"
            CREATE TABLE IF NOT EXISTS audit_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                execution_id TEXT NOT NULL,
                timestamp TEXT NOT NULL,
                command TEXT NOT NULL,
                subcommand TEXT,
                tool_name TEXT,
                target_type TEXT,
                target_id TEXT,
                role TEXT NOT NULL,
                status TEXT NOT NULL,
                exit_code INTEGER NOT NULL,
                duration_ms INTEGER NOT NULL,
                working_directory TEXT NOT NULL,
                arguments_json TEXT,
                stdout_truncated TEXT,
                stderr_truncated TEXT,
                error_message TEXT,
                host TEXT,
                pid INTEGER NOT NULL,
                session_id TEXT,
                task_id TEXT,
                job_run_id TEXT,
                activity_id TEXT,
                step_index INTEGER
            );

            CREATE INDEX IF NOT EXISTS idx_audit_events_timestamp
            ON audit_events(timestamp);

            CREATE INDEX IF NOT EXISTS idx_audit_events_tool_name
            ON audit_events(tool_name);

            CREATE INDEX IF NOT EXISTS idx_audit_events_status
            ON audit_events(status);

            CREATE INDEX IF NOT EXISTS idx_audit_events_role
            ON audit_events(role);

            CREATE INDEX IF NOT EXISTS idx_audit_events_target
            ON audit_events(target_type, target_id);

            CREATE UNIQUE INDEX IF NOT EXISTS idx_audit_events_execution_id
            ON audit_events(execution_id);
        "#,
    )
    .map_err(|e| OrbitError::Store(e.to_string()))?;

    add_column_if_missing(conn, "ALTER TABLE audit_events ADD COLUMN task_id TEXT")?;
    add_column_if_missing(conn, "ALTER TABLE audit_events ADD COLUMN job_run_id TEXT")?;
    add_column_if_missing(conn, "ALTER TABLE audit_events ADD COLUMN activity_id TEXT")?;
    add_column_if_missing(
        conn,
        "ALTER TABLE audit_events ADD COLUMN step_index INTEGER",
    )?;

    conn.execute_batch(
        r#"
            CREATE INDEX IF NOT EXISTS idx_audit_events_task_id
            ON audit_events(task_id);

            CREATE INDEX IF NOT EXISTS idx_audit_events_job_run_id
            ON audit_events(job_run_id);
        "#,
    )
    .map_err(|e| OrbitError::Store(e.to_string()))?;

    Ok(())
}

/// v7 `trusted_mcp_audit_provenance` migration (ORB-10228): additive trusted
/// workspace, caller/process, transport, capability-set, session/call, and
/// lease correlation for command-audit rows. Existing rows remain untouched
/// and therefore read with NULL/empty additions.
pub(super) fn apply_trusted_mcp_audit_provenance(conn: &Connection) -> Result<(), OrbitError> {
    for sql in [
        "ALTER TABLE audit_events ADD COLUMN workspace_id TEXT",
        "ALTER TABLE audit_events ADD COLUMN caller_machine_id TEXT",
        "ALTER TABLE audit_events ADD COLUMN caller_host_id TEXT",
        "ALTER TABLE audit_events ADD COLUMN process_machine_id TEXT",
        "ALTER TABLE audit_events ADD COLUMN process_host_id TEXT",
        "ALTER TABLE audit_events ADD COLUMN transport TEXT",
        "ALTER TABLE audit_events ADD COLUMN capabilities_json TEXT",
        "ALTER TABLE audit_events ADD COLUMN origin_session_id TEXT",
        "ALTER TABLE audit_events ADD COLUMN mcp_call_id TEXT",
        "ALTER TABLE audit_events ADD COLUMN lease_id TEXT",
    ] {
        add_column_if_missing(conn, sql)?;
    }

    conn.execute_batch(
        r#"
            CREATE INDEX IF NOT EXISTS idx_audit_events_workspace_id
            ON audit_events(workspace_id);

            CREATE INDEX IF NOT EXISTS idx_audit_events_caller_machine_id
            ON audit_events(caller_machine_id);

            CREATE INDEX IF NOT EXISTS idx_audit_events_process_machine_id
            ON audit_events(process_machine_id);

            CREATE INDEX IF NOT EXISTS idx_audit_events_transport
            ON audit_events(transport);

            CREATE INDEX IF NOT EXISTS idx_audit_events_origin_session_id
            ON audit_events(origin_session_id);

            CREATE INDEX IF NOT EXISTS idx_audit_events_mcp_call_id
            ON audit_events(mcp_call_id);

            CREATE INDEX IF NOT EXISTS idx_audit_events_lease_id
            ON audit_events(lease_id);
        "#,
    )
    .map_err(|error| OrbitError::Store(error.to_string()))
}

/// Add transport-neutral invocation correlation to command-audit rows.
/// Existing rows and non-invocation producers remain NULL-compatible.
pub(super) fn apply_invocation_audit_context(conn: &Connection) -> Result<(), OrbitError> {
    // A ledger may outlive an incomplete pre-ledger schema (for example, an
    // invocation-only fixture). Re-establish the canonical audit table and
    // its existing provenance projection before extending it.
    ensure_audit_events_schema(conn)?;
    apply_trusted_mcp_audit_provenance(conn)?;

    add_column_if_missing(conn, "ALTER TABLE audit_events ADD COLUMN trace_id TEXT")?;
    add_column_if_missing(conn, "ALTER TABLE audit_events ADD COLUMN caller_ip TEXT")?;

    conn.execute_batch(
        r#"
            CREATE INDEX IF NOT EXISTS idx_audit_events_trace_id
            ON audit_events(trace_id);
        "#,
    )
    .map_err(|error| OrbitError::Store(error.to_string()))
}

/// v16 `audit_actor_identity` migration (ORB-10888): materialize the canonical
/// actor identity beside the overloaded `role` label, then backfill it for
/// every existing row so a 30d/90d window stays comparable across the change.
///
/// `role` itself is never read back into, rewritten, or reinterpreted here —
/// trust classification keeps reading exactly the bytes it read before.
pub(super) fn apply_audit_actor_identity(conn: &Connection) -> Result<(), OrbitError> {
    ensure_audit_events_schema(conn)?;

    for sql in [
        "ALTER TABLE audit_events ADD COLUMN actor_kind TEXT",
        "ALTER TABLE audit_events ADD COLUMN actor_id TEXT",
        "ALTER TABLE audit_events ADD COLUMN actor_vendor TEXT",
        "ALTER TABLE audit_events ADD COLUMN actor_family TEXT",
        "ALTER TABLE audit_events ADD COLUMN actor_model TEXT",
        "ALTER TABLE audit_events ADD COLUMN actor_alias_version INTEGER",
    ] {
        add_column_if_missing(conn, sql)?;
    }

    conn.execute_batch(
        r#"
            CREATE INDEX IF NOT EXISTS idx_audit_events_actor
            ON audit_events(actor_kind, actor_id);
        "#,
    )
    .map_err(|error| OrbitError::Store(error.to_string()))?;

    backfill_audit_actor_identity(conn)
}

/// v17 `audit_self_reported_actor` migration (ORB-10890): record the identity
/// an unauthenticated caller claims for itself in a column of its own.
///
/// Strictly additive and deliberately **not** backfilled. Every existing row
/// was written before any claim was collected, so there is no claim to recover;
/// deriving one from `role` would retroactively attribute traffic Orbit never
/// authenticated. Existing `unverified` rows therefore stay valid and read as
/// anonymous.
pub(super) fn apply_audit_self_reported_actor(conn: &Connection) -> Result<(), OrbitError> {
    ensure_audit_events_schema(conn)?;

    add_column_if_missing(
        conn,
        "ALTER TABLE audit_events ADD COLUMN self_reported_actor TEXT",
    )?;

    conn.execute_batch(
        r#"
            CREATE INDEX IF NOT EXISTS idx_audit_events_self_reported_actor
            ON audit_events(self_reported_actor);
        "#,
    )
    .map_err(|error| OrbitError::Store(error.to_string()))
}

/// v18 `audit_actor_alias_v2` migration: the alias map moved `fable` from a
/// shorthand entry to a family rule, so versioned Fable labels (`fable-5.1`)
/// now resolve to `claude` instead of an unrecognized family. Re-derive every
/// row stamped with the previous map; rows already at the current version are
/// untouched, and `role` is never rewritten.
pub(super) fn apply_audit_actor_alias_v2(conn: &Connection) -> Result<(), OrbitError> {
    ensure_audit_events_schema(conn)?;
    backfill_audit_actor_identity(conn)
}

/// Derive the actor columns for every row whose stamped alias version is not
/// the current one.
///
/// The mapping runs through the same Rust alias map the insert path uses, keyed
/// on `SELECT DISTINCT role` — a handful of labels, not a row-by-row pass — so
/// a new model never requires an SQL edit here. A later alias-map version bump
/// re-runs this from its own append-only ledger entry.
pub(crate) fn backfill_audit_actor_identity(conn: &Connection) -> Result<(), OrbitError> {
    let labels = {
        let mut stmt = conn
            .prepare(
                "SELECT DISTINCT role FROM audit_events \
                 WHERE actor_alias_version IS NULL OR actor_alias_version != ?1",
            )
            .map_err(|error| OrbitError::Store(error.to_string()))?;
        let rows = stmt
            .query_map([ACTOR_ALIAS_MAP_VERSION], |row| row.get::<_, String>(0))
            .map_err(|error| OrbitError::Store(error.to_string()))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| OrbitError::Store(error.to_string()))?
    };

    for label in labels {
        let actor = canonical_actor_for_role_label(&label);
        conn.execute(
            "UPDATE audit_events SET actor_kind = ?1, actor_id = ?2, actor_vendor = ?3, \
             actor_family = ?4, actor_model = ?5, actor_alias_version = ?6 \
             WHERE role = ?7 AND (actor_alias_version IS NULL OR actor_alias_version != ?6)",
            rusqlite::params![
                actor.kind.as_str(),
                actor.id,
                actor.vendor,
                actor.family,
                actor.model,
                actor.alias_version,
                label,
            ],
        )
        .map_err(|error| OrbitError::Store(error.to_string()))?;
    }

    Ok(())
}

/// v25 `audit_plugin_grants` migration: the grant set (JSON array of grant
/// names) a plugin-backed tool call ran under, beside the three provenance
/// columns (design `docs/design/plugins/1_scope.md` §4.4). Additive.
pub(super) fn apply_audit_plugin_grants(conn: &Connection) -> Result<(), OrbitError> {
    ensure_audit_events_schema(conn)?;
    add_column_if_missing(
        conn,
        "ALTER TABLE audit_events ADD COLUMN plugin_grants TEXT",
    )
}

/// v23 `audit_machine_name_columns` migration (ORB-12725): *host* is reserved
/// for the MCP-host/process sense, so the two audit columns that carry a
/// machine's display name are renamed to say so. A rename rather than an
/// additive column plus backfill, because one column is one fact and two
/// spellings of it would drift.
pub(super) fn apply_audit_machine_name_columns(conn: &Connection) -> Result<(), OrbitError> {
    if !table_exists(conn, "audit_events")? {
        return Ok(());
    }
    for (old, new) in [
        ("caller_host_id", "caller_machine_name"),
        ("process_host_id", "process_machine_name"),
    ] {
        if !table_has_column(conn, "audit_events", old)?
            || table_has_column(conn, "audit_events", new)?
        {
            continue;
        }
        conn.execute(
            &format!("ALTER TABLE audit_events RENAME COLUMN {old} TO {new}"),
            [],
        )
        .map_err(|e| OrbitError::Store(e.to_string()))?;
    }
    Ok(())
}
