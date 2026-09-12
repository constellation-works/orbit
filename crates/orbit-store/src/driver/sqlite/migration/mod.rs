use std::path::Path;

use orbit_common::OrbitError;
use orbit_types::telemetry::{ACTOR_ALIAS_MAP_VERSION, canonical_actor_for_role_label};
use rusqlite::Connection;

mod feature;
mod ledger;

pub use feature::{
    AppliedFeatureMigration, FeatureMigration, FeatureSchemaStatus, PendingFeatureMigration,
};
pub use ledger::{AppliedMigration, SUPPORTED_SCHEMA_VERSION};
pub(crate) use ledger::{applied_migrations, current_schema_version};

/// Bring the store database up to the newest supported schema version,
/// applying any pending versioned migrations (each transactional and
/// recorded in the `schema_meta` ledger). Refuses to open a database whose
/// recorded schema version is newer than this binary supports.
pub(crate) fn apply_schema(conn: &Connection) -> Result<(), OrbitError> {
    ledger::run_migrations(conn, ledger::MIGRATIONS)
}

pub(crate) fn apply_schema_at_path(conn: &Connection, path: &Path) -> Result<(), OrbitError> {
    ledger::run_migrations_at_path(conn, ledger::MIGRATIONS, path)
}

/// Registry metadata for one schema migration not yet recorded as applied,
/// as surfaced by `orbit migrate --dry-run` (ORB-10012).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingSchemaMigration {
    pub version: u32,
    pub name: &'static str,
}

/// Read-only view of a store database's migration ledger: the recorded
/// schema version plus the registry migrations still pending against it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaLedgerStatus {
    /// Schema version recorded in the ledger (0 for a fresh/pre-ledger or
    /// nonexistent database).
    pub current_version: u32,
    /// Registry migrations newer than `current_version`, in apply order.
    /// Empty when the database is current or newer than this binary
    /// (compare against [`SUPPORTED_SCHEMA_VERSION`] to distinguish).
    pub pending: Vec<PendingSchemaMigration>,
}

/// Inspect the migration ledger of the store database at `db_path` without
/// opening it for writing — and therefore without triggering the automatic
/// migrations that [`crate::Store::open`] applies. A missing database reads
/// as version 0 with every registry migration pending (opening it would
/// create and migrate it). Powers `orbit migrate --dry-run`.
pub fn read_schema_ledger_status(db_path: &Path) -> Result<SchemaLedgerStatus, OrbitError> {
    let current_version = if db_path.exists() {
        let conn = Connection::open_with_flags(
            db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(|e| {
            OrbitError::Store(format!(
                "cannot open store database '{}' read-only: {e}",
                db_path.display()
            ))
        })?;
        current_schema_version(&conn)?
    } else {
        0
    };

    Ok(SchemaLedgerStatus {
        current_version,
        pending: pending_schema_migrations_after(current_version),
    })
}

/// Registry migrations newer than `current_version`, in apply order. Lets
/// callers that already know the recorded version (e.g. via
/// [`crate::Store::schema_version`]) list what is pending without another
/// database open.
pub fn pending_schema_migrations_after(current_version: u32) -> Vec<PendingSchemaMigration> {
    ledger::MIGRATIONS
        .iter()
        .filter(|m| m.version > current_version)
        .map(|m| PendingSchemaMigration {
            version: m.version,
            name: m.name,
        })
        .collect()
}

/// v1 `baseline` migration: the full pre-ledger idempotent schema. Safe to
/// run on both a fresh database and any legacy database created by the
/// pre-versioning migration code, which is how existing databases adopt
/// the versioned ledger.
///
/// ADR-0287: this shipped structure is immutable. Later schema belongs in a
/// new migration so databases that already recorded v1 receive it too.
fn apply_baseline_schema(conn: &Connection) -> Result<(), OrbitError> {
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

/// v2 `learnings_index_workspace_scope` migration (ORB-10113): re-key the
/// learning envelope index by `(workspace_id, id)`.
///
/// The index previously had no workspace discriminator and was keyed only by
/// learning ID, so in the shared host-global database rows written by one
/// workspace leaked into another workspace's searches and reminders. YAML
/// under each `.orbit/learnings/` is the source of truth, and legacy rows
/// cannot be attributed to a workspace reliably, so this migration discards
/// every indexed row and lets each runtime rebuild its own rows from YAML via
/// `sync_learnings`. It touches only SQLite — no `learning.yaml` file is read
/// or modified.
fn apply_learning_index_workspace_scope(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        r#"
            DROP TABLE IF EXISTS learnings_index;

            CREATE TABLE learnings_index (
                workspace_id TEXT NOT NULL,
                id           TEXT NOT NULL,
                status       TEXT NOT NULL,
                paths        TEXT NOT NULL,
                tags         TEXT NOT NULL,
                summary      TEXT NOT NULL,
                updated_at   TEXT NOT NULL,
                priority     INTEGER,
                PRIMARY KEY (workspace_id, id)
            );

            CREATE INDEX IF NOT EXISTS learnings_active
                ON learnings_index(workspace_id, status) WHERE status = 'active';
        "#,
    )
    .map_err(|e| OrbitError::Store(e.to_string()))
}

fn ensure_routine_schema(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        r#"
            -- Host-local routine scheduler state [ORB-10021]. Lives only in
            -- the host-global store database and is never synced between
            -- hosts (ADR-0208); routine *definitions* are git-versioned YAML
            -- in routine-source workspaces.

            -- Per-routine cursor: first observation baseline + last slot
            -- consumed. A routine never fires for slots before its baseline.
            CREATE TABLE IF NOT EXISTS routine_cursors (
                routine_name TEXT PRIMARY KEY,
                baseline_at TEXT NOT NULL,
                last_slot TEXT,
                updated_at TEXT NOT NULL
            );

            -- One row per fire attempt. The (name, slot, attempt) uniqueness
            -- is the idempotency key that prevents double fires for the same
            -- scheduled slot across overlapping or crashed sweeps.
            CREATE TABLE IF NOT EXISTS routine_fires (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                routine_name TEXT NOT NULL,
                slot TEXT NOT NULL,
                attempt INTEGER NOT NULL DEFAULT 1,
                state TEXT NOT NULL,
                run_id TEXT,
                source_workspace TEXT NOT NULL,
                detail TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                UNIQUE(routine_name, slot, attempt)
            );

            CREATE INDEX IF NOT EXISTS idx_routine_fires_name_slot
            ON routine_fires(routine_name, slot DESC, attempt DESC);

            CREATE INDEX IF NOT EXISTS idx_routine_fires_state
            ON routine_fires(state);

            -- Host-local suppressions written by `orbit routine pause`;
            -- durable across reboots, invisible to git.
            CREATE TABLE IF NOT EXISTS routine_pauses (
                routine_name TEXT PRIMARY KEY,
                paused_at TEXT NOT NULL,
                actor TEXT
            );
        "#,
    )
    .map_err(|e| OrbitError::Store(e.to_string()))
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

fn ensure_learning_index_schema(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        r#"
            -- Project-learnings envelope index. YAML records live on disk under
            -- `<root>/<id>/learning.yaml`; status lives in the YAML body.
            -- this table indexes the envelope fields for fast scope-glob
            -- lookups. Arrays are stored as JSON strings for the same reason
            -- the ADR index does it: phase-1 corpora are small and a junction
            -- table is overkill. Per ADR-004, ranking and FTS over body
            -- content are deferred to phase 2.
            CREATE TABLE IF NOT EXISTS learnings_index (
                id          TEXT PRIMARY KEY,
                status      TEXT NOT NULL,
                paths       TEXT NOT NULL,
                tags        TEXT NOT NULL,
                summary     TEXT NOT NULL,
                updated_at  TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS learnings_active
                ON learnings_index(status) WHERE status = 'active';
        "#,
    )
    .map_err(|e| OrbitError::Store(e.to_string()))?;

    // C2 (T20260511-6) adds an optional `priority` column used as the
    // secondary ranking key in `search`. NULL is acceptable; the search
    // path orders Some(N) ahead of None and falls back to updated_at.
    add_column_if_missing(
        conn,
        "ALTER TABLE learnings_index ADD COLUMN priority INTEGER",
    )?;

    Ok(())
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

fn add_column_if_missing(conn: &Connection, sql: &str) -> Result<(), OrbitError> {
    match conn.execute(sql, []) {
        Ok(_) => Ok(()),
        Err(e) if e.to_string().contains("duplicate column name") => Ok(()),
        Err(e) => Err(OrbitError::Store(e.to_string())),
    }
}

fn ensure_tools_schema(conn: &Connection) -> Result<(), OrbitError> {
    add_column_if_missing(
        conn,
        "ALTER TABLE tools ADD COLUMN parameters_json TEXT NOT NULL DEFAULT '[]'",
    )?;
    add_column_if_missing(
        conn,
        "ALTER TABLE tools ADD COLUMN enabled INTEGER NOT NULL DEFAULT 1",
    )?;
    add_column_if_missing(
        conn,
        "ALTER TABLE tools ADD COLUMN builtin INTEGER NOT NULL DEFAULT 0",
    )?;
    add_column_if_missing(
        conn,
        "ALTER TABLE tools ADD COLUMN created_at TEXT NOT NULL DEFAULT ''",
    )?;
    add_column_if_missing(
        conn,
        "ALTER TABLE tools ADD COLUMN updated_at TEXT NOT NULL DEFAULT ''",
    )?;

    if table_has_column(conn, "tools", "is_enabled")? {
        conn.execute(
            r#"
                UPDATE tools
                SET enabled = CASE
                    WHEN lower(CAST(is_enabled AS TEXT)) IN ('0', 'false', 'f', 'no') THEN 0
                    ELSE 1
                END
            "#,
            [],
        )
        .map_err(|e| OrbitError::Store(e.to_string()))?;
    }

    if table_has_column(conn, "tools", "is_builtin")? {
        conn.execute(
            r#"
                UPDATE tools
                SET builtin = CASE
                    WHEN lower(CAST(is_builtin AS TEXT)) IN ('1', 'true', 't', 'yes') THEN 1
                    ELSE 0
                END
            "#,
            [],
        )
        .map_err(|e| OrbitError::Store(e.to_string()))?;
    }

    conn.execute(
        "UPDATE tools SET parameters_json = '[]' WHERE parameters_json = ''",
        [],
    )
    .map_err(|e| OrbitError::Store(e.to_string()))?;
    conn.execute(
        "UPDATE tools SET created_at = datetime('now') WHERE created_at = ''",
        [],
    )
    .map_err(|e| OrbitError::Store(e.to_string()))?;
    conn.execute(
        "UPDATE tools SET updated_at = datetime('now') WHERE updated_at = ''",
        [],
    )
    .map_err(|e| OrbitError::Store(e.to_string()))?;

    Ok(())
}

fn ensure_audit_events_schema(conn: &Connection) -> Result<(), OrbitError> {
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

/// v10 `invocation_telemetry_columns` migration (ORB-10367): re-run the
/// idempotent invocation-schema step against databases that already recorded
/// the v1 baseline.
///
/// The 5m/1h cache split (`cache_create_1h_tokens`) and the token-derived
/// cost column (`provider_cost_usd`) were added to
/// [`ensure_invocation_schema`], which only ever runs as part of the v1
/// `baseline` migration. Every database created before those columns landed
/// is already at v1 or newer, so `run_migrations` skips baseline and the
/// `ALTER`s never reach it — the insert then binds columns the table lacks
/// and every agent-dispatching run dies at the telemetry write. Registering
/// the same idempotent step under its own version is what carries it to
/// existing databases.
fn apply_invocation_telemetry_columns(conn: &Connection) -> Result<(), OrbitError> {
    // The `ALTER`s below address tables the v1 baseline creates. A database
    // without them has nothing to repair (and `ALTER TABLE` on a missing
    // table is an error, not a no-op), so skip rather than fail the open.
    if !table_exists(conn, "invocations")?
        || !table_exists(conn, "invocation_tasks")?
        || !table_exists(conn, "tool_calls")?
    {
        return Ok(());
    }
    add_column_if_missing(
        conn,
        "ALTER TABLE invocations ADD COLUMN provider_cost_usd REAL",
    )?;
    add_column_if_missing(
        conn,
        "ALTER TABLE invocations ADD COLUMN cache_create_1h_tokens INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_invocation_schema_v1(conn)
}

fn ensure_invocation_schema_v1(conn: &Connection) -> Result<(), OrbitError> {
    add_column_if_missing(conn, "ALTER TABLE invocations ADD COLUMN slot TEXT")?;
    conn.execute_batch(
        r#"
            CREATE INDEX IF NOT EXISTS idx_invocations_job_run_id
            ON invocations(job_run_id);

            CREATE INDEX IF NOT EXISTS idx_invocations_activity_id
            ON invocations(activity_id);

            CREATE INDEX IF NOT EXISTS idx_invocations_ts
            ON invocations(ts DESC, id DESC);

            CREATE INDEX IF NOT EXISTS idx_invocation_tasks_task_id
            ON invocation_tasks(task_id);

            CREATE INDEX IF NOT EXISTS idx_tool_calls_tool_name
            ON tool_calls(tool_name);
        "#,
    )
    .map_err(|e| OrbitError::Store(e.to_string()))
}

fn ensure_v2_state_consolidation_schema(conn: &Connection) -> Result<(), OrbitError> {
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

fn apply_flat_crew_model(conn: &Connection) -> Result<(), OrbitError> {
    // ADR-0213: keep the legacy role columns nullable for existing databases;
    // new reads fall back to implementer_model when crew_model is not populated.
    add_column_if_missing(conn, "ALTER TABLE job_runs ADD COLUMN crew_model TEXT")
}

fn apply_job_run_archive_stage(conn: &Connection) -> Result<(), OrbitError> {
    add_column_if_missing(conn, "ALTER TABLE job_runs ADD COLUMN archived_at TEXT")
}

/// v11 `routine_scheduler_schema` migration (ORB-10462): routine tables were
/// added only to the mutable v1 baseline after the ledger shipped. Register
/// the idempotent schema step so existing databases receive the same tables
/// as fresh databases before the baseline is frozen by ADR-0287.
fn apply_routine_scheduler_schema(conn: &Connection) -> Result<(), OrbitError> {
    ensure_routine_schema(conn)
}

/// v5 `host_registry_core` migration (ORB-10255): durable machine identity,
/// lifecycle state, and immutable historical names in the hub-global store.
///
/// This migration is strictly additive. Cross-table triggers backstop the
/// typed API's preflight so a current or tombstoned `host_id` can never exist
/// for two machines, even when the database is modified outside that API.
fn apply_host_registry_core(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        r#"
            CREATE TABLE IF NOT EXISTS hosts (
                machine_id    TEXT PRIMARY KEY,
                host_id       TEXT NOT NULL UNIQUE,
                labels_json   TEXT NOT NULL DEFAULT '[]',
                status        TEXT NOT NULL CHECK (status IN ('active', 'retired')),
                registered_at TEXT NOT NULL,
                updated_at    TEXT NOT NULL,
                retired_at    TEXT,
                last_seen_at  TEXT,
                CHECK (length(machine_id) > 0),
                CHECK (length(host_id) > 0),
                CHECK (json_valid(labels_json) AND json_type(labels_json) = 'array'),
                CHECK (
                    (status = 'active' AND retired_at IS NULL)
                    OR (status = 'retired' AND retired_at IS NOT NULL)
                )
            );

            CREATE TABLE IF NOT EXISTS host_aliases (
                host_id    TEXT PRIMARY KEY,
                machine_id TEXT NOT NULL,
                created_at TEXT NOT NULL,
                warning    TEXT NOT NULL,
                CHECK (length(host_id) > 0),
                CHECK (length(warning) > 0),
                FOREIGN KEY(machine_id) REFERENCES hosts(machine_id)
                    ON UPDATE RESTRICT ON DELETE RESTRICT
            );

            CREATE INDEX IF NOT EXISTS idx_hosts_status_host_id
                ON hosts(status, host_id);
            CREATE INDEX IF NOT EXISTS idx_host_aliases_machine_id
                ON host_aliases(machine_id, created_at);

            CREATE TRIGGER IF NOT EXISTS hosts_host_id_not_alias_insert
            BEFORE INSERT ON hosts
            WHEN EXISTS (
                SELECT 1 FROM host_aliases WHERE host_id = NEW.host_id
            )
            BEGIN
                SELECT RAISE(ABORT, 'host_id is reserved by a permanent alias');
            END;

            CREATE TRIGGER IF NOT EXISTS hosts_host_id_not_alias_update
            BEFORE UPDATE OF host_id ON hosts
            WHEN EXISTS (
                SELECT 1 FROM host_aliases WHERE host_id = NEW.host_id
            )
            BEGIN
                SELECT RAISE(ABORT, 'host_id is reserved by a permanent alias');
            END;

            CREATE TRIGGER IF NOT EXISTS host_alias_not_current_name_insert
            BEFORE INSERT ON host_aliases
            WHEN EXISTS (
                SELECT 1 FROM hosts WHERE host_id = NEW.host_id
            )
            BEGIN
                SELECT RAISE(ABORT, 'host alias conflicts with a current host_id');
            END;

            CREATE TRIGGER IF NOT EXISTS host_aliases_immutable_update
            BEFORE UPDATE ON host_aliases
            BEGIN
                SELECT RAISE(ABORT, 'host aliases are immutable');
            END;

            CREATE TRIGGER IF NOT EXISTS host_aliases_immutable_delete
            BEFORE DELETE ON host_aliases
            BEGIN
                SELECT RAISE(ABORT, 'host aliases are permanent');
            END;
        "#,
    )
    .map_err(|error| OrbitError::Store(error.to_string()))
}

/// v6 `workspace_coordination_projections` migration (ORB-10257): singular
/// workspace ownership, private host-keyed presence, and owner-published
/// execution profiles. The schema is additive and keeps owner payload JSON
/// separate from hub-owned generation/receipt metadata.
fn apply_workspace_coordination_projections(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        r#"
            CREATE TABLE IF NOT EXISTS workspace_ownership (
                workspace_id     TEXT PRIMARY KEY,
                owner_machine_id TEXT NOT NULL,
                bound_at         TEXT NOT NULL,
                updated_at       TEXT NOT NULL,
                CHECK (length(workspace_id) > 0),
                FOREIGN KEY(owner_machine_id) REFERENCES hosts(machine_id)
                    ON UPDATE RESTRICT ON DELETE RESTRICT
            );

            CREATE INDEX IF NOT EXISTS idx_workspace_ownership_owner
                ON workspace_ownership(owner_machine_id, workspace_id);

            CREATE TABLE IF NOT EXISTS host_workspace_presence (
                machine_id   TEXT NOT NULL,
                workspace_id TEXT NOT NULL,
                root         TEXT NOT NULL,
                last_verified TEXT NOT NULL,
                PRIMARY KEY(machine_id, workspace_id),
                CHECK (length(workspace_id) > 0),
                CHECK (length(root) > 0),
                FOREIGN KEY(machine_id) REFERENCES hosts(machine_id)
                    ON UPDATE RESTRICT ON DELETE RESTRICT
            );

            CREATE INDEX IF NOT EXISTS idx_host_workspace_presence_workspace
                ON host_workspace_presence(workspace_id, machine_id);

            CREATE TABLE IF NOT EXISTS workspace_execution_profiles (
                workspace_id     TEXT PRIMARY KEY,
                owner_machine_id TEXT NOT NULL,
                generation       INTEGER NOT NULL CHECK (generation >= 1),
                payload_json     TEXT NOT NULL,
                received_at      TEXT NOT NULL,
                CHECK (json_valid(payload_json) AND json_type(payload_json) = 'object'),
                FOREIGN KEY(workspace_id) REFERENCES workspace_ownership(workspace_id)
                    ON UPDATE RESTRICT ON DELETE RESTRICT,
                FOREIGN KEY(owner_machine_id) REFERENCES hosts(machine_id)
                    ON UPDATE RESTRICT ON DELETE RESTRICT
            );

            CREATE TRIGGER IF NOT EXISTS execution_profile_owner_matches_insert
            BEFORE INSERT ON workspace_execution_profiles
            WHEN NOT EXISTS (
                SELECT 1 FROM workspace_ownership o
                WHERE o.workspace_id = NEW.workspace_id
                  AND o.owner_machine_id = NEW.owner_machine_id
            )
            BEGIN
                SELECT RAISE(ABORT, 'execution profile owner does not match workspace ownership');
            END;

            CREATE TRIGGER IF NOT EXISTS execution_profile_owner_matches_update
            BEFORE UPDATE OF workspace_id, owner_machine_id ON workspace_execution_profiles
            WHEN NOT EXISTS (
                SELECT 1 FROM workspace_ownership o
                WHERE o.workspace_id = NEW.workspace_id
                  AND o.owner_machine_id = NEW.owner_machine_id
            )
            BEGIN
                SELECT RAISE(ABORT, 'execution profile owner does not match workspace ownership');
            END;
        "#,
    )
    .map_err(|error| OrbitError::Store(error.to_string()))
}

/// v7 `trusted_mcp_audit_provenance` migration (ORB-10228): additive trusted
/// workspace, caller/process, transport, capability-set, session/call, and
/// lease correlation for command-audit rows. Existing rows remain untouched
/// and therefore read with NULL/empty additions.
fn apply_trusted_mcp_audit_provenance(conn: &Connection) -> Result<(), OrbitError> {
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

/// v8 `hub_registry_metadata` migration (ORB-10267): one singleton hub-global
/// metadata row carrying the configured hub `machine_id` and a monotonic
/// `registry_revision`. Every snapshot-visible host/alias/retirement,
/// ownership, presence, or execution-profile mutation advances the revision
/// exactly once inside its own transaction; per-workspace execution-profile
/// generation remains a separate concept. The `id = 0` primary-key CHECK is
/// the SQLite singleton guard, and the row is seeded here so every reader sees
/// a revision without a lazy insert path.
fn apply_hub_registry_metadata(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        r#"
            CREATE TABLE IF NOT EXISTS hub_registry_metadata (
                id                INTEGER PRIMARY KEY CHECK (id = 0),
                hub_machine_id    TEXT,
                registry_revision INTEGER NOT NULL DEFAULT 0
                    CHECK (
                        typeof(registry_revision) = 'integer'
                        AND registry_revision >= 0
                        AND registry_revision <= 9223372036854775807
                    ),
                updated_at        TEXT NOT NULL,
                CHECK (hub_machine_id IS NULL OR length(hub_machine_id) > 0)
            );

            INSERT OR IGNORE INTO hub_registry_metadata(
                id, hub_machine_id, registry_revision, updated_at
            ) VALUES (0, NULL, 0, datetime('now'));
        "#,
    )
    .map_err(|error| OrbitError::Store(error.to_string()))
}

/// v9 `feature_schema_ledger` migration (ORB-10319): generic append-only
/// migration ledger for vertical feature crates. Feature versions advance
/// independently from the Store-global schema after this one-time ownership
/// boundary and are recorded transactionally with their feature-owned DDL.
fn apply_feature_schema_ledger(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        r#"
            CREATE TABLE IF NOT EXISTS feature_schema_meta (
                feature    TEXT NOT NULL,
                version    INTEGER NOT NULL,
                name       TEXT NOT NULL,
                applied_at TEXT NOT NULL,
                PRIMARY KEY(feature, version),
                CHECK (length(feature) > 0 AND feature = trim(feature)),
                CHECK (
                    typeof(version) = 'integer'
                    AND version > 0
                    AND version <= 4294967295
                ),
                CHECK (length(name) > 0 AND name = trim(name)),
                CHECK (length(applied_at) > 0)
            );

            CREATE TRIGGER IF NOT EXISTS feature_schema_meta_immutable_update
            BEFORE UPDATE ON feature_schema_meta
            BEGIN
                SELECT RAISE(ABORT, 'feature schema migration records are immutable');
            END;

            CREATE TRIGGER IF NOT EXISTS feature_schema_meta_immutable_delete
            BEFORE DELETE ON feature_schema_meta
            BEGIN
                SELECT RAISE(ABORT, 'feature schema migration records are append-only');
            END;
        "#,
    )
    .map_err(|error| OrbitError::Store(error.to_string()))
}

/// v12 `friction_records_sqlite` migration (ORB-10680): hub friction records
/// move from the per-workspace Markdown tree into the host-global store.
///
/// Identity is composite `(workspace_id, friction_id)` (L-0072): friction IDs
/// stay workspace-local, so the same `F2026-05-001` in two workspaces are two
/// distinct rows. Indexes cover the read shapes the scan path used to satisfy
/// by parsing the whole corpus — workspace + status + creation order, and
/// workspace + tag.
///
/// `legacy_path` is the read-only evidence pointer for an imported record and
/// is NULL for anything written after cutover (ADR-0345).
fn apply_friction_records_schema(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        r#"
            CREATE TABLE IF NOT EXISTS friction_records (
                workspace_id     TEXT NOT NULL,
                friction_id      TEXT NOT NULL,
                month            TEXT NOT NULL,
                seq              INTEGER NOT NULL,
                title            TEXT,
                model            TEXT NOT NULL,
                status           TEXT NOT NULL,
                created_at       TEXT NOT NULL,
                resolved_at      TEXT,
                during_task      TEXT,
                resolved_by_task TEXT,
                tags_json        TEXT NOT NULL DEFAULT '[]',
                body             TEXT NOT NULL,
                legacy_path      TEXT,
                PRIMARY KEY(workspace_id, friction_id),
                CHECK (length(workspace_id) > 0 AND workspace_id = trim(workspace_id)),
                CHECK (length(friction_id) > 0 AND friction_id = trim(friction_id)),
                CHECK (length(month) = 7),
                CHECK (typeof(seq) = 'integer' AND seq > 0),
                CHECK (status IN ('open', 'triaged', 'resolved')),
                CHECK (length(model) > 0),
                CHECK (length(created_at) > 0),
                CHECK (json_valid(tags_json) AND json_type(tags_json) = 'array')
            );

            CREATE UNIQUE INDEX IF NOT EXISTS idx_friction_records_month_seq
                ON friction_records(workspace_id, month, seq);
            CREATE INDEX IF NOT EXISTS idx_friction_records_status_created
                ON friction_records(workspace_id, status, created_at, friction_id);
            CREATE INDEX IF NOT EXISTS idx_friction_records_created
                ON friction_records(workspace_id, created_at, friction_id);

            CREATE TABLE IF NOT EXISTS friction_record_tags (
                workspace_id TEXT NOT NULL,
                friction_id  TEXT NOT NULL,
                tag          TEXT NOT NULL,
                PRIMARY KEY(workspace_id, friction_id, tag),
                CHECK (length(tag) > 0 AND tag = trim(tag)),
                FOREIGN KEY(workspace_id, friction_id)
                    REFERENCES friction_records(workspace_id, friction_id)
                    ON UPDATE CASCADE ON DELETE CASCADE
            );

            CREATE INDEX IF NOT EXISTS idx_friction_record_tags_lookup
                ON friction_record_tags(workspace_id, tag, friction_id);

            CREATE TABLE IF NOT EXISTS friction_import_state (
                workspace_id   TEXT NOT NULL,
                source_key     TEXT NOT NULL,
                record_count   INTEGER NOT NULL,
                imported_count INTEGER NOT NULL,
                schema_version INTEGER NOT NULL,
                completed_at   TEXT NOT NULL,
                PRIMARY KEY(workspace_id, source_key),
                CHECK (length(workspace_id) > 0),
                CHECK (typeof(record_count) = 'integer' AND record_count >= 0),
                CHECK (typeof(imported_count) = 'integer' AND imported_count >= 0),
                CHECK (typeof(schema_version) = 'integer' AND schema_version > 0),
                CHECK (length(completed_at) > 0)
            );
        "#,
    )
    .map_err(|error| OrbitError::Store(error.to_string()))
}

/// v13 `workspace_claim_scope` migration (ORB-10709, ADR-0352).
///
/// The exclusive workspace claim reuses `task_reservations` rather than a
/// parallel table, because atomic acquisition, TTL, lazy expiry, audit, and a
/// release escape hatch already live there. `scope` is the dimension that keeps
/// the two apart — worker file reservations arbitrate over paths, the claim
/// arbitrates dispatch authority — and every existing row is a file
/// reservation, which is exactly what the column default says.
///
/// `claim_token` is the holder's bearer token. It is deliberately absent from
/// `select_reservation_columns()`, so no reservation read path can carry it
/// into a response.
fn apply_workspace_claim_scope(conn: &Connection) -> Result<(), OrbitError> {
    // A database that recorded a ledger version without ever running the
    // baseline has no reservation table to widen. Create it at its baseline
    // shape first so this migration widens a table that exists rather than
    // wedging the ledger; on every ordinary database this is a no-op.
    ensure_task_reservations_schema(conn)?;

    add_column_if_missing(
        conn,
        "ALTER TABLE task_reservations ADD COLUMN scope TEXT NOT NULL DEFAULT 'files'",
    )?;
    add_column_if_missing(
        conn,
        "ALTER TABLE task_reservations ADD COLUMN claim_token TEXT",
    )?;

    conn.execute_batch(
        r#"
            CREATE INDEX IF NOT EXISTS idx_task_reservations_scope_release
            ON task_reservations(workspace_id, scope, released_at);
        "#,
    )
    .map_err(|e| OrbitError::Store(e.to_string()))?;

    Ok(())
}

/// v14 `remove_native_learning_subsystem` (ORB-10736): remove every SQLite
/// projection owned by the retired native learning resource. Existing files
/// under `.orbit/learnings/` are deliberately outside the database migration
/// and remain untouched as inert historical data.
fn apply_remove_native_learning_subsystem(conn: &Connection) -> Result<(), OrbitError> {
    if table_exists(conn, "embeddings")? && table_has_column(conn, "embeddings", "source_kind")? {
        conn.execute("DELETE FROM embeddings WHERE source_kind = 'learning'", [])
            .map_err(|error| OrbitError::Store(error.to_string()))?;
    }
    conn.execute_batch(
        r#"
            DROP TABLE IF EXISTS learnings_index;
            DROP TABLE IF EXISTS session_learning_state;
            DROP TABLE IF EXISTS id_allocations;
        "#,
    )
    .map_err(|error| OrbitError::Store(error.to_string()))
}

/// Add transport-neutral invocation correlation to command-audit rows.
/// Existing rows and non-invocation producers remain NULL-compatible.
fn apply_invocation_audit_context(conn: &Connection) -> Result<(), OrbitError> {
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
fn apply_audit_actor_identity(conn: &Connection) -> Result<(), OrbitError> {
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
fn apply_audit_self_reported_actor(conn: &Connection) -> Result<(), OrbitError> {
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
fn apply_audit_actor_alias_v2(conn: &Connection) -> Result<(), OrbitError> {
    ensure_audit_events_schema(conn)?;
    backfill_audit_actor_identity(conn)
}

/// v19 `job_runs_created_index`: cover the listing's per-workspace
/// `ORDER BY created_at DESC, run_id ASC` so a bounded page stops scanning
/// and sorting the whole workspace history.
///
/// A legacy database may reach this entry with no `job_runs` table, or with
/// the pre-consolidation one that has no `workspace_id`; the current table
/// is created at open time by `ensure_v2_state_consolidation_schema`, which
/// declares the same index. So this entry indexes the table only when it is
/// already the current shape and otherwise leaves it to open time.
fn apply_job_runs_created_index(conn: &Connection) -> Result<(), OrbitError> {
    if !table_has_column(conn, "job_runs", "workspace_id")?
        || !table_has_column(conn, "job_runs", "created_at")?
    {
        return Ok(());
    }
    conn.execute_batch(
        r#"
            CREATE INDEX IF NOT EXISTS idx_job_runs_workspace_created
            ON job_runs(workspace_id, created_at DESC, run_id ASC);
        "#,
    )
    .map_err(|error| OrbitError::Store(error.to_string()))
}

/// v20 `invocations_ts_index`: cover the accounting window filters and the
/// newest-first invocation listing on `invocations(ts, id)`.
///
/// Guarded like v19: `ensure_invocation_schema_v1` declares the same index
/// for a database whose `invocations` table is created or upgraded at open.
fn apply_invocations_ts_index(conn: &Connection) -> Result<(), OrbitError> {
    if !table_has_column(conn, "invocations", "ts")? {
        return Ok(());
    }
    conn.execute_batch(
        r#"
            CREATE INDEX IF NOT EXISTS idx_invocations_ts
            ON invocations(ts DESC, id DESC);
        "#,
    )
    .map_err(|error| OrbitError::Store(error.to_string()))
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

fn ensure_task_reservations_schema(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        r#"
            CREATE TABLE IF NOT EXISTS task_reservations (
                reservation_id TEXT PRIMARY KEY,
                workspace_orbit_dir TEXT NOT NULL,
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

            CREATE INDEX IF NOT EXISTS idx_task_reservations_workspace_expires
            ON task_reservations(workspace_orbit_dir, expires_at);

            CREATE INDEX IF NOT EXISTS idx_task_reservations_workspace_release
            ON task_reservations(workspace_orbit_dir, released_at);
        "#,
    )
    .map_err(|e| OrbitError::Store(e.to_string()))?;

    add_column_if_missing(
        conn,
        "ALTER TABLE task_reservations ADD COLUMN workspace_id TEXT",
    )?;
    add_column_if_missing(
        conn,
        "ALTER TABLE task_reservations ADD COLUMN owner_run_id TEXT",
    )?;
    add_column_if_missing(
        conn,
        "ALTER TABLE task_reservations ADD COLUMN owner_metadata_json TEXT",
    )?;
    add_column_if_missing(
        conn,
        "ALTER TABLE task_reservations ADD COLUMN release_reason TEXT",
    )?;
    add_column_if_missing(
        conn,
        "ALTER TABLE task_reservations ADD COLUMN release_metadata_json TEXT",
    )?;

    conn.execute_batch(
        r#"
            CREATE INDEX IF NOT EXISTS idx_task_reservations_workspace_owner_release
            ON task_reservations(workspace_orbit_dir, owner_run_id, released_at);

            CREATE INDEX IF NOT EXISTS idx_task_reservations_workspace_id_release
            ON task_reservations(workspace_id, released_at);

            CREATE INDEX IF NOT EXISTS idx_task_reservations_workspace_id_expires
            ON task_reservations(workspace_id, expires_at);
        "#,
    )
    .map_err(|e| OrbitError::Store(e.to_string()))?;

    Ok(())
}

fn table_exists(conn: &Connection, table: &str) -> Result<bool, OrbitError> {
    let exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name = ?1",
            [table],
            |row| row.get(0),
        )
        .map_err(|e| OrbitError::Store(e.to_string()))?;
    Ok(exists > 0)
}

fn table_has_column(conn: &Connection, table: &str, column: &str) -> Result<bool, OrbitError> {
    let pragma = format!("PRAGMA table_info({table})");
    let mut stmt = conn
        .prepare(&pragma)
        .map_err(|e| OrbitError::Store(e.to_string()))?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| OrbitError::Store(e.to_string()))?;

    for name in rows {
        let name = name.map_err(|e| OrbitError::Store(e.to_string()))?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn table_has_foreign_key_to(
    conn: &Connection,
    table: &str,
    referenced_table: &str,
) -> Result<bool, OrbitError> {
    let pragma = format!("PRAGMA foreign_key_list({table})");
    let mut stmt = conn
        .prepare(&pragma)
        .map_err(|e| OrbitError::Store(e.to_string()))?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(2))
        .map_err(|e| OrbitError::Store(e.to_string()))?;

    for name in rows {
        let name = name.map_err(|e| OrbitError::Store(e.to_string()))?;
        if name == referenced_table {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests;
