use orbit_common::OrbitError;
use rusqlite::Connection;

use super::audit_events::ensure_audit_events_schema;
use super::introspect::{add_column_if_missing, table_exists, table_has_foreign_key_to};
use super::invocation::ensure_invocation_schema_v1;
use super::learning::ensure_learning_index_schema;
use super::task_reservations::ensure_task_reservations_schema;
use super::tools::ensure_tools_schema;

/// v1 `baseline` migration: the full pre-ledger idempotent schema. Safe to
/// run on both a fresh database and any legacy database created by the
/// pre-versioning migration code, which is how existing databases adopt
/// the versioned ledger.
///
/// ADR-0287: this shipped structure is immutable. Later schema belongs in a
/// new migration so databases that already recorded v1 receive it too.
pub(super) fn apply_baseline_schema(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        r#"
            CREATE TABLE IF NOT EXISTS tools (
                name TEXT PRIMARY KEY,
                path TEXT NOT NULL,
                description TEXT NOT NULL DEFAULT '',
                parameters_json TEXT NOT NULL DEFAULT '[]',
                enabled INTEGER NOT NULL DEFAULT 1,
                builtin INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS agent_sessions (
                session_id TEXT PRIMARY KEY,
                task_id TEXT NOT NULL,
                identity_id TEXT,
                identity_name TEXT,
                identity_role TEXT,
                identity_block TEXT,
                skill_names TEXT NOT NULL,
                composed_context_hash TEXT NOT NULL,
                effective_allowed_tools TEXT NOT NULL,
                tool_calls TEXT NOT NULL,
                outcome TEXT NOT NULL,
                status TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

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

            CREATE TABLE IF NOT EXISTS task_reservations (
                reservation_id TEXT PRIMARY KEY,
                workspace_orbit_dir TEXT NOT NULL,
                workspace_id TEXT,
                task_ids_json TEXT NOT NULL,
                files_json TEXT NOT NULL,
                actor TEXT NOT NULL,
                created_at TEXT NOT NULL,
                expires_at TEXT NOT NULL,
                released_at TEXT,
                owner_run_id TEXT,
                owner_metadata_json TEXT,
                release_reason TEXT,
                release_metadata_json TEXT
            );

            CREATE TABLE IF NOT EXISTS invocations (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                ts TEXT NOT NULL,
                job_run_id TEXT NOT NULL,
                activity_id TEXT NOT NULL,
                agent TEXT NOT NULL,
                model TEXT,
                slot TEXT,
                duration_ms INTEGER NOT NULL DEFAULT 0,
                input_tokens INTEGER NOT NULL DEFAULT 0,
                cache_read_tokens INTEGER NOT NULL DEFAULT 0,
                cache_create_tokens INTEGER NOT NULL DEFAULT 0,
                output_tokens INTEGER NOT NULL DEFAULT 0,
                tool_call_count INTEGER NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS invocation_tasks (
                invocation_id INTEGER NOT NULL,
                task_id TEXT NOT NULL,
                PRIMARY KEY(invocation_id, task_id),
                FOREIGN KEY(invocation_id) REFERENCES invocations(id) ON DELETE CASCADE
            );

            CREATE TABLE IF NOT EXISTS tool_calls (
                invocation_id INTEGER NOT NULL,
                seq INTEGER NOT NULL,
                tool_name TEXT NOT NULL,
                result_bytes INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY(invocation_id, seq),
                FOREIGN KEY(invocation_id) REFERENCES invocations(id) ON DELETE CASCADE
            );

            -- ADR envelope index. Bodies live on disk under <root>/<state>/<id>/body.md;
            -- this table indexes the YAML envelope fields for filter queries.
            -- Arrays (related_features, related_tasks, tags, paths, legacy_ids,
            -- supersedes, validation_warnings) are stored as JSON-encoded strings
            -- so filters can use `LIKE '%"<value>"%'` until the corpus warrants junction
            -- tables. FTS5 over body content is owned by `orbit-search::vector`,
            -- not this schema.
            CREATE TABLE IF NOT EXISTS adrs (
                id TEXT PRIMARY KEY,
                status TEXT NOT NULL,
                title TEXT NOT NULL,
                owner TEXT NOT NULL,
                related_features TEXT NOT NULL DEFAULT '[]',
                related_tasks TEXT NOT NULL DEFAULT '[]',
                tags TEXT NOT NULL DEFAULT '[]',
                paths TEXT NOT NULL DEFAULT '[]',
                legacy_ids TEXT NOT NULL DEFAULT '[]',
                supersedes TEXT NOT NULL DEFAULT '[]',
                superseded_by TEXT,
                validation_warnings TEXT NOT NULL DEFAULT '[]',
                legacy_validation TEXT NOT NULL DEFAULT 'none',
                created_at TEXT NOT NULL,
                accepted_at TEXT,
                last_updated TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_adrs_status ON adrs(status);
            CREATE INDEX IF NOT EXISTS idx_adrs_owner ON adrs(owner);
        "#,
    )
    .map_err(|e| OrbitError::Store(e.to_string()))?;

    ensure_agent_sessions_schema(conn)?;
    ensure_tools_schema(conn)?;
    ensure_adr_index_schema(conn)?;
    ensure_audit_events_schema(conn)?;
    ensure_task_reservations_schema(conn)?;
    ensure_learning_index_schema(conn)?;
    ensure_invocation_schema_v1(conn)?;
    ensure_v2_state_consolidation_schema(conn)?;

    Ok(())
}

fn ensure_adr_index_schema(conn: &Connection) -> Result<(), OrbitError> {
    add_column_if_missing(
        conn,
        "ALTER TABLE adrs ADD COLUMN tags TEXT NOT NULL DEFAULT '[]'",
    )?;
    add_column_if_missing(
        conn,
        "ALTER TABLE adrs ADD COLUMN paths TEXT NOT NULL DEFAULT '[]'",
    )
}

fn ensure_agent_sessions_schema(conn: &Connection) -> Result<(), OrbitError> {
    if table_exists(conn, "agent_sessions")?
        && table_has_foreign_key_to(conn, "agent_sessions", "tasks")?
    {
        conn.execute_batch(
            r#"
                ALTER TABLE agent_sessions RENAME TO agent_sessions_legacy;

                CREATE TABLE agent_sessions (
                    session_id TEXT PRIMARY KEY,
                    task_id TEXT NOT NULL,
                    identity_id TEXT,
                    identity_name TEXT,
                    identity_role TEXT,
                    identity_block TEXT,
                    skill_names TEXT NOT NULL,
                    composed_context_hash TEXT NOT NULL,
                    effective_allowed_tools TEXT NOT NULL,
                    tool_calls TEXT NOT NULL,
                    outcome TEXT NOT NULL,
                    status TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );

                INSERT INTO agent_sessions(
                    session_id, task_id, identity_id, identity_name, identity_role, identity_block, skill_names, composed_context_hash, effective_allowed_tools,
                    tool_calls, outcome, status, created_at, updated_at
                )
                SELECT
                    session_id, task_id, NULL, NULL, NULL, NULL, skill_names, composed_context_hash, effective_allowed_tools,
                    tool_calls, outcome, status, created_at, updated_at
                FROM agent_sessions_legacy;

                DROP TABLE agent_sessions_legacy;
            "#,
        )
        .map_err(|e| OrbitError::Store(e.to_string()))?;
    }

    add_column_if_missing(
        conn,
        "ALTER TABLE agent_sessions ADD COLUMN identity_id TEXT",
    )?;
    add_column_if_missing(
        conn,
        "ALTER TABLE agent_sessions ADD COLUMN identity_name TEXT",
    )?;
    add_column_if_missing(
        conn,
        "ALTER TABLE agent_sessions ADD COLUMN identity_role TEXT",
    )?;
    add_column_if_missing(
        conn,
        "ALTER TABLE agent_sessions ADD COLUMN identity_block TEXT",
    )?;

    Ok(())
}

pub(super) fn ensure_v2_state_consolidation_schema(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        r#"
            CREATE TABLE IF NOT EXISTS v2_audit_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                workspace_id TEXT NOT NULL,
                event_id TEXT NOT NULL,
                source TEXT NOT NULL,
                schema_version INTEGER NOT NULL,
                event_type TEXT NOT NULL,
                ts TEXT NOT NULL,
                run_id TEXT NOT NULL,
                agent_identity TEXT NOT NULL,
                parent_event_id TEXT,
                workspace_path TEXT,
                payload_json TEXT NOT NULL,
                UNIQUE(workspace_id, event_id)
            );

            CREATE INDEX IF NOT EXISTS idx_v2_audit_events_ws_ts
            ON v2_audit_events(workspace_id, ts);

            CREATE INDEX IF NOT EXISTS idx_v2_audit_events_ws_run
            ON v2_audit_events(workspace_id, run_id, ts);

            CREATE INDEX IF NOT EXISTS idx_v2_audit_events_ws_event_type
            ON v2_audit_events(workspace_id, event_type);

            CREATE TABLE IF NOT EXISTS job_runs (
                run_id TEXT NOT NULL,
                workspace_id TEXT NOT NULL,
                job_id TEXT NOT NULL,
                attempt INTEGER NOT NULL,
                state TEXT NOT NULL,
                scheduled_at TEXT NOT NULL,
                started_at TEXT,
                finished_at TEXT,
                duration_ms INTEGER,
                created_at TEXT NOT NULL,
                pid INTEGER,
                pid_start_time TEXT,
                input_json TEXT,
                retry_source_run_id TEXT,
                knowledge_metrics_json TEXT,
                resolved_crew TEXT,
                planner_model TEXT,
                implementer_model TEXT,
                reviewer_model TEXT,
                pipeline_state_json TEXT,
                PRIMARY KEY(workspace_id, run_id)
            );

            CREATE INDEX IF NOT EXISTS idx_job_runs_ws_job_sched
            ON job_runs(workspace_id, job_id, scheduled_at DESC);

            CREATE INDEX IF NOT EXISTS idx_job_runs_ws_state
            ON job_runs(workspace_id, state);

            CREATE INDEX IF NOT EXISTS idx_job_runs_workspace_created
            ON job_runs(workspace_id, created_at DESC, run_id ASC);

            CREATE TABLE IF NOT EXISTS job_run_steps (
                workspace_id TEXT NOT NULL,
                run_id TEXT NOT NULL,
                step_index INTEGER NOT NULL,
                target_type TEXT NOT NULL,
                target_id TEXT NOT NULL,
                state TEXT NOT NULL,
                started_at TEXT,
                finished_at TEXT,
                duration_ms INTEGER,
                exit_code INTEGER,
                error_code TEXT,
                error_message TEXT,
                agent_response_json TEXT,
                PRIMARY KEY(workspace_id, run_id, step_index),
                FOREIGN KEY(workspace_id, run_id)
                    REFERENCES job_runs(workspace_id, run_id)
                    ON DELETE CASCADE
            );

            CREATE TABLE IF NOT EXISTS session_learning_state (
                workspace_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                learning_injection_state_json TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                PRIMARY KEY(workspace_id, session_id)
            );

            CREATE INDEX IF NOT EXISTS idx_session_learning_state_ws
            ON session_learning_state(workspace_id, updated_at);

            CREATE TABLE IF NOT EXISTS schema_meta (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
        "#,
    )
    .map_err(|e| OrbitError::Store(e.to_string()))
}
