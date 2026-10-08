// ORB-10003: versioned schema-migration ledger.
use std::path::Path;
use std::sync::{Arc, Barrier};
use std::thread;

use orbit_common::OrbitError;

use crate::contracts::{
    BreakingMigration, COMPATIBILITY_RECORD_FORMAT, CompatibilityRecord, MigrationCompatibility,
};
use rusqlite::Connection;

use super::super::ledger::{self, Migration};
use super::super::*;

fn ledger_rows(conn: &Connection) -> Vec<(String, String)> {
    let mut stmt = conn
        .prepare("SELECT key, value FROM schema_meta WHERE key LIKE 'migration.v%' ORDER BY key")
        .expect("prepare ledger query");
    stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("query ledger rows")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect ledger rows")
}

#[test]
fn legacy_db_adopts_versioned_ledger() {
    let conn = Connection::open_in_memory().expect("open in-memory connection");

    // Schema as the pre-ledger idempotent migrations would have left an
    // old database: legacy `tools` shape, `adrs` without tags/paths, and
    // an `agent_sessions` with a foreign key to `tasks` (the shape the
    // rename-copy-drop migration rewrites). No schema_meta table at all.
    conn.execute_batch(
        r#"
            CREATE TABLE tools (
                name TEXT PRIMARY KEY,
                path TEXT NOT NULL,
                description TEXT NOT NULL DEFAULT '',
                is_enabled INTEGER NOT NULL DEFAULT 1,
                is_builtin INTEGER NOT NULL DEFAULT 0
            );
            INSERT INTO tools(name, path, description, is_enabled, is_builtin)
            VALUES ('legacy-tool', '/bin/legacy', 'legacy tool', 0, 1);

            CREATE TABLE adrs (
                id TEXT PRIMARY KEY,
                status TEXT NOT NULL,
                title TEXT NOT NULL,
                owner TEXT NOT NULL,
                related_features TEXT NOT NULL DEFAULT '[]',
                related_tasks TEXT NOT NULL DEFAULT '[]',
                legacy_ids TEXT NOT NULL DEFAULT '[]',
                supersedes TEXT NOT NULL DEFAULT '[]',
                superseded_by TEXT,
                validation_warnings TEXT NOT NULL DEFAULT '[]',
                legacy_validation TEXT NOT NULL DEFAULT 'none',
                created_at TEXT NOT NULL,
                accepted_at TEXT,
                last_updated TEXT NOT NULL
            );

            CREATE TABLE tasks (id TEXT PRIMARY KEY);
            INSERT INTO tasks(id) VALUES ('T1');
            CREATE TABLE agent_sessions (
                session_id TEXT PRIMARY KEY,
                task_id TEXT NOT NULL,
                skill_names TEXT NOT NULL,
                composed_context_hash TEXT NOT NULL,
                effective_allowed_tools TEXT NOT NULL,
                tool_calls TEXT NOT NULL,
                outcome TEXT NOT NULL,
                status TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                FOREIGN KEY(task_id) REFERENCES tasks(id)
            );
            INSERT INTO agent_sessions VALUES (
                's1', 'T1', '[]', 'hash', '[]', '[]', 'ok', 'done',
                '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z'
            );
        "#,
    )
    .expect("create legacy schema");

    apply_schema(&conn).expect("adopt legacy db");

    // Baseline ran idempotently: new columns exist and legacy data survived.
    assert!(table_has_column(&conn, "tools", "enabled").expect("enabled column"));
    assert!(table_has_column(&conn, "adrs", "tags").expect("tags column"));
    assert!(table_has_column(&conn, "agent_sessions", "identity_id").expect("identity column"));
    let enabled: i64 = conn
        .query_row(
            "SELECT enabled FROM tools WHERE name = 'legacy-tool'",
            [],
            |row| row.get(0),
        )
        .expect("query migrated tool");
    assert_eq!(enabled, 0);
    let session_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM agent_sessions WHERE session_id = 's1'",
            [],
            |row| row.get(0),
        )
        .expect("query migrated session");
    assert_eq!(session_count, 1);

    // ...and the ledger now records the adoption.
    assert_eq!(
        current_schema_version(&conn).expect("current version"),
        SUPPORTED_SCHEMA_VERSION
    );
    assert_eq!(
        ledger_rows(&conn),
        vec![
            ("migration.v0001".to_string(), "baseline".to_string()),
            (
                "migration.v0002".to_string(),
                "learnings_index_workspace_scope".to_string()
            ),
            ("migration.v0003".to_string(), "flat_crew_model".to_string()),
            (
                "migration.v0004".to_string(),
                "job_run_archive_stage".to_string()
            ),
            (
                "migration.v0005".to_string(),
                "host_registry_core".to_string()
            ),
            (
                "migration.v0006".to_string(),
                "workspace_coordination_projections".to_string()
            ),
            (
                "migration.v0007".to_string(),
                "trusted_mcp_audit_provenance".to_string()
            ),
            (
                "migration.v0008".to_string(),
                "hub_registry_metadata".to_string()
            ),
            (
                "migration.v0009".to_string(),
                "feature_schema_ledger".to_string()
            ),
            (
                "migration.v0010".to_string(),
                "invocation_telemetry_columns".to_string()
            ),
            (
                "migration.v0011".to_string(),
                "routine_scheduler_schema".to_string()
            ),
            (
                "migration.v0012".to_string(),
                "friction_records_sqlite".to_string()
            ),
            (
                "migration.v0013".to_string(),
                "workspace_claim_scope".to_string()
            ),
            (
                "migration.v0014".to_string(),
                "remove_native_learning_subsystem".to_string()
            ),
            (
                "migration.v0015".to_string(),
                "invocation_audit_context".to_string()
            ),
            (
                "migration.v0016".to_string(),
                "audit_actor_identity".to_string()
            ),
            (
                "migration.v0017".to_string(),
                "audit_self_reported_actor".to_string()
            ),
            (
                "migration.v0018".to_string(),
                "audit_actor_alias_v2".to_string()
            ),
            (
                "migration.v0019".to_string(),
                "job_runs_created_index".to_string()
            ),
            (
                "migration.v0020".to_string(),
                "invocations_ts_index".to_string()
            ),
            (
                "migration.v0021".to_string(),
                "task_commit_journal".to_string()
            ),
            (
                "migration.v0022".to_string(),
                "execution_provenance".to_string()
            ),
            (
                "migration.v0023".to_string(),
                "audit_machine_name_columns".to_string()
            ),
            (
                "migration.v0024".to_string(),
                "plugins_and_audit_plugin_provenance".to_string()
            ),
            (
                "migration.v0025".to_string(),
                "audit_plugin_grants".to_string()
            ),
            (
                "migration.v0026".to_string(),
                "remove_operation_mode".to_string()
            ),
            (
                "migration.v0027".to_string(),
                "plugin_certified_orbit_version".to_string()
            ),
            (
                "migration.v0028".to_string(),
                "plugin_archive_digest".to_string()
            ),
            (
                "migration.v0029".to_string(),
                "friction_rehome_target".to_string()
            ),
            (
                "migration.v0030".to_string(),
                "audit_plugin_secrets".to_string()
            ),
            (
                "migration.v0031".to_string(),
                "audit_plugin_secret_updates".to_string()
            ),
            (
                "migration.v0032".to_string(),
                "audit_brokered_call".to_string()
            ),
            (
                "migration.v0033".to_string(),
                "job_run_id_allocations".to_string()
            ),
            (
                "migration.v0034".to_string(),
                "job_runs_job_created_and_retry_indexes".to_string()
            ),
            (
                "migration.v0035".to_string(),
                "plugin_build_record".to_string()
            ),
            (
                "migration.v0036".to_string(),
                "invocation_workspace_scope".to_string()
            ),
            (
                "migration.v0037".to_string(),
                "audit_tool_call_index".to_string()
            ),
            ("migration.v0038".to_string(), "job_run_states".to_string()),
            (
                "migration.v0039".to_string(),
                "job_runs_recency_index".to_string()
            ),
            (
                "migration.v0040".to_string(),
                "provider_limit_observations".to_string()
            ),
        ]
    );
}

/// Stamp a database as a newer binary would have left it: an extra ledger
/// row plus the forward-compatibility record describing that version.
fn stamp_newer_database(conn: &Connection, version: u32, breaking: &[(u32, &str)]) {
    stamp_newer_database_with(conn, version, breaking, None);
}

/// [`stamp_newer_database`], with the writer classification a binary that
/// covers older writers records (`None` is a record from before it did).
fn stamp_newer_database_with(
    conn: &Connection,
    version: u32,
    breaking: &[(u32, &str)],
    read_only: Option<&[(u32, &str)]>,
) {
    conn.execute(
        "INSERT INTO schema_meta(key, value, updated_at) VALUES (?1, 'from-the-future', ?2)",
        rusqlite::params![format!("migration.v{version:04}"), "2099-01-01T00:00:00Z"],
    )
    .expect("record future migration");
    let record = CompatibilityRecord {
        format: COMPATIBILITY_RECORD_FORMAT,
        version,
        breaking: breaking
            .iter()
            .map(|(version, name)| BreakingMigration {
                version: *version,
                name: (*name).to_string(),
            })
            .collect(),
        read_only: read_only.map(|entries| {
            entries
                .iter()
                .map(|(version, name)| BreakingMigration {
                    version: *version,
                    name: (*name).to_string(),
                })
                .collect()
        }),
    };
    conn.execute(
        "INSERT INTO schema_meta(key, value, updated_at) VALUES ('migration.compat', ?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![
            record.encode().expect("encode record"),
            "2099-01-01T00:00:00Z"
        ],
    )
    .expect("record compatibility metadata");
}

#[test]
fn breaking_newer_database_refuses_and_names_the_first_missing_migration() {
    let conn = Connection::open_in_memory().expect("open in-memory connection");
    apply_schema(&conn).expect("apply schema");
    stamp_newer_database(
        &conn,
        SUPPORTED_SCHEMA_VERSION + 2,
        &[
            (SUPPORTED_SCHEMA_VERSION + 1, "split_job_runs"),
            (SUPPORTED_SCHEMA_VERSION + 2, "drop_audit_events"),
        ],
    );

    let err = apply_schema(&conn).expect_err("must refuse a breaking newer schema");
    let message = err.to_string();
    assert!(
        message.contains(&format!("schema version {}", SUPPORTED_SCHEMA_VERSION + 2)),
        "{message}"
    );
    assert!(
        message.contains(&format!(
            "v{} (split_job_runs)",
            SUPPORTED_SCHEMA_VERSION + 1
        )),
        "{message}"
    );
    assert!(!message.contains("drop_audit_events"), "{message}");
    assert!(message.contains("upgrade orbit"), "{message}");
}

fn migration_v1_marker(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch("CREATE TABLE ledger_test_v1 (x INTEGER)")
        .map_err(|e| OrbitError::Store(e.to_string()))
}

fn migration_v2_fails_midway(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        "CREATE TABLE ledger_test_half_applied (x INTEGER);\n         this is not valid migration SQL",
    )
        .map_err(|e| OrbitError::Store(e.to_string()))?;
    Ok(())
}

#[test]
fn failed_migration_rolls_back_schema_and_ledger() {
    let conn = Connection::open_in_memory().expect("open in-memory connection");
    let registry = [
        Migration {
            version: 1,
            name: "marker",
            compat: MigrationCompatibility::Additive,
            apply: migration_v1_marker,
        },
        Migration {
            version: 2,
            name: "fails-midway",
            compat: MigrationCompatibility::Additive,
            apply: migration_v2_fails_midway,
        },
    ];

    let err = ledger::run_migrations(&conn, &registry).expect_err("v2 must fail");
    assert!(matches!(err, OrbitError::Migration(_)), "got {err:?}");
    let message = err.to_string();
    assert!(message.contains("v2 (fails-midway)"), "got {message}");

    // v1 committed; v2 rolled back completely — no half-applied schema,
    // no ledger row.
    assert!(table_exists(&conn, "ledger_test_v1").expect("v1 table"));
    assert!(!table_exists(&conn, "ledger_test_half_applied").expect("v2 table rolled back"));
    assert_eq!(current_schema_version(&conn).expect("current version"), 1);
    assert_eq!(ledger_rows(&conn).len(), 1);

    // A fixed registry can resume from where the ledger left off.
    let fixed = [
        Migration {
            version: 1,
            name: "marker",
            compat: MigrationCompatibility::Additive,
            apply: migration_v1_marker,
        },
        Migration {
            version: 2,
            name: "fixed",
            compat: MigrationCompatibility::Additive,
            apply: migration_v1_marker_v2,
        },
    ];
    ledger::run_migrations(&conn, &fixed).expect("resume after fix");
    assert_eq!(current_schema_version(&conn).expect("current version"), 2);
}

fn seed_pre_ledger_fixture(path: &Path) {
    let conn = Connection::open(path).expect("seed pre-ledger db");
    conn.execute_batch(
        "CREATE TABLE tools (
            name TEXT PRIMARY KEY,
            path TEXT NOT NULL,
            description TEXT NOT NULL DEFAULT '',
            is_enabled INTEGER NOT NULL DEFAULT 1,
            is_builtin INTEGER NOT NULL DEFAULT 0
        );",
    )
    .expect("pre-ledger tools table");
    conn.pragma_update(None, "journal_mode", "WAL")
        .expect("enable WAL on fixture");
}

fn assert_full_unique_ledger(applied: &[AppliedMigration]) {
    assert_eq!(applied.len(), SUPPORTED_SCHEMA_VERSION as usize);
    for (index, row) in applied.iter().enumerate() {
        assert_eq!(row.version, u32::try_from(index).expect("index") + 1);
    }
}

#[test]
fn concurrent_store_open_on_pre_ledger_fixture_applies_each_version_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("orbit.db");
    seed_pre_ledger_fixture(&path);

    let barrier = Arc::new(Barrier::new(2));
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let path = path.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                crate::Store::open(&path)
            })
        })
        .collect();

    let stores: Vec<_> = handles
        .into_iter()
        .map(|handle| {
            handle
                .join()
                .expect("join")
                .expect("concurrent Store::open")
        })
        .collect();

    for store in &stores {
        let applied = store.applied_migrations().expect("applied migrations");
        assert_full_unique_ledger(&applied);
    }
}

fn migration_v1_marker_v2(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch("CREATE TABLE ledger_test_v2 (x INTEGER)")
        .map_err(|e| OrbitError::Store(e.to_string()))
}

fn schema_column_fingerprint(conn: &Connection) -> String {
    let mut table_statement = conn
        .prepare(
            "SELECT name FROM sqlite_master
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
             ORDER BY name",
        )
        .expect("prepare table list");
    let tables = table_statement
        .query_map([], |row| row.get::<_, String>(0))
        .expect("query table list")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect table list");

    tables
        .into_iter()
        .map(|table| {
            let mut column_statement = conn
                .prepare(&format!("PRAGMA table_info({table})"))
                .expect("prepare column list");
            let columns = column_statement
                .query_map([], |row| row.get::<_, String>(1))
                .expect("query column list")
                .collect::<Result<Vec<_>, _>>()
                .expect("collect column list");
            format!("{table}:{}\n", columns.join(","))
        })
        .collect()
}

/// Frozen DDL fixture of the shipped v1 baseline schema.
///
/// Real databases created at v1 recorded `migration.v0001` with this schema.
/// New schema must arrive via append-only ledger migrations, not edits to
/// `apply_baseline_schema`. Seeding the legacy test database from this frozen
/// fixture ensures that changes to `apply_baseline_schema` cannot silently
/// mask missing ledger migrations.
const SHIPPED_V1_SCHEMA: &str = r#"
CREATE TABLE tools (
    name TEXT PRIMARY KEY,
    path TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    parameters_json TEXT NOT NULL DEFAULT '[]',
    enabled INTEGER NOT NULL DEFAULT 1,
    builtin INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

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

CREATE TABLE audit_events (
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

CREATE TABLE task_reservations (
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

CREATE TABLE invocations (
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

CREATE TABLE invocation_tasks (
    invocation_id INTEGER NOT NULL,
    task_id TEXT NOT NULL,
    PRIMARY KEY(invocation_id, task_id),
    FOREIGN KEY(invocation_id) REFERENCES invocations(id) ON DELETE CASCADE
);

CREATE TABLE tool_calls (
    invocation_id INTEGER NOT NULL,
    seq INTEGER NOT NULL,
    tool_name TEXT NOT NULL,
    result_bytes INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY(invocation_id, seq),
    FOREIGN KEY(invocation_id) REFERENCES invocations(id) ON DELETE CASCADE
);

CREATE TABLE adrs (
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

CREATE TABLE v2_audit_events (
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

CREATE TABLE job_runs (
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

CREATE TABLE job_run_steps (
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

CREATE TABLE session_learning_state (
    workspace_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    learning_injection_state_json TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY(workspace_id, session_id)
);

CREATE TABLE learnings_index (
    id          TEXT PRIMARY KEY,
    status      TEXT NOT NULL,
    paths       TEXT NOT NULL,
    tags        TEXT NOT NULL,
    summary     TEXT NOT NULL,
    updated_at  TEXT NOT NULL,
    priority    INTEGER
);

CREATE TABLE schema_meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

INSERT INTO schema_meta(key, value, updated_at) VALUES
    ('migration.v0001', 'baseline', '2026-07-01T00:00:00Z');

CREATE INDEX idx_adrs_status ON adrs(status);
CREATE INDEX idx_adrs_owner ON adrs(owner);
CREATE INDEX idx_v2_audit_events_ws_ts ON v2_audit_events(workspace_id, ts);
CREATE INDEX idx_v2_audit_events_ws_run ON v2_audit_events(workspace_id, run_id, ts);
CREATE INDEX idx_v2_audit_events_ws_event_type ON v2_audit_events(workspace_id, event_type);
CREATE INDEX idx_job_runs_ws_job_sched ON job_runs(workspace_id, job_id, scheduled_at DESC);
CREATE INDEX idx_job_runs_ws_state ON job_runs(workspace_id, state);
CREATE INDEX idx_job_runs_workspace_created ON job_runs(workspace_id, created_at DESC, run_id ASC);
CREATE INDEX idx_session_learning_state_ws ON session_learning_state(workspace_id, updated_at);
CREATE INDEX idx_audit_events_timestamp ON audit_events(timestamp);
CREATE INDEX idx_audit_events_tool_name ON audit_events(tool_name);
CREATE INDEX idx_audit_events_status ON audit_events(status);
CREATE INDEX idx_audit_events_role ON audit_events(role);
CREATE INDEX idx_audit_events_target ON audit_events(target_type, target_id);
CREATE UNIQUE INDEX idx_audit_events_execution_id ON audit_events(execution_id);
CREATE INDEX idx_audit_events_task_id ON audit_events(task_id);
CREATE INDEX idx_audit_events_job_run_id ON audit_events(job_run_id);
CREATE INDEX idx_task_reservations_workspace_expires ON task_reservations(workspace_orbit_dir, expires_at);
CREATE INDEX idx_task_reservations_workspace_release ON task_reservations(workspace_orbit_dir, released_at);
CREATE INDEX idx_task_reservations_workspace_owner_release ON task_reservations(workspace_orbit_dir, owner_run_id, released_at);
CREATE INDEX idx_task_reservations_workspace_id_release ON task_reservations(workspace_id, released_at);
CREATE INDEX idx_task_reservations_workspace_id_expires ON task_reservations(workspace_id, expires_at);
CREATE INDEX learnings_active ON learnings_index(status) WHERE status = 'active';
CREATE INDEX idx_invocations_job_run_id ON invocations(job_run_id);
CREATE INDEX idx_invocations_activity_id ON invocations(activity_id);
CREATE INDEX idx_invocations_ts ON invocations(ts DESC, id DESC);
CREATE INDEX idx_invocation_tasks_task_id ON invocation_tasks(task_id);
CREATE INDEX idx_tool_calls_tool_name ON tool_calls(tool_name);
"#;

/// A database that recorded shipped v1 and then advances through every
/// registered migration must expose the same table/column structure as a
/// database created fresh by the current binary.
#[test]
fn v1_upgrade_and_fresh_database_have_identical_columns() {
    let fresh = Connection::open_in_memory().expect("open fresh database");
    apply_schema(&fresh).expect("migrate fresh database");

    let legacy = Connection::open_in_memory().expect("open legacy database");
    legacy
        .execute_batch(SHIPPED_V1_SCHEMA)
        .expect("seed frozen shipped v1 database");
    ledger::run_migrations(&legacy, ledger::MIGRATIONS).expect("upgrade through registry");

    assert_eq!(
        applied_migrations(&legacy)
            .expect("legacy migration ledger")
            .len(),
        ledger::MIGRATIONS.len()
    );
    assert_eq!(
        schema_column_fingerprint(&legacy),
        schema_column_fingerprint(&fresh)
    );
}

/// v36 attributes a legacy invocation only when exactly one workspace ever
/// held its run id; a colliding or orphaned run id leaves it unattributed so
/// no workspace-scoped read picks it up.
#[test]
fn invocation_workspace_backfill_attributes_only_unambiguous_runs() {
    let conn = Connection::open_in_memory().expect("open legacy database");
    let before = ledger::MIGRATIONS
        .iter()
        .position(|m| m.name == "invocation_workspace_scope")
        .expect("v36 registered");
    ledger::run_migrations(&conn, &ledger::MIGRATIONS[..before]).expect("migrate to v35");
    conn.execute_batch(
        r#"
            INSERT INTO job_run_id_allocations(workspace_id, run_id) VALUES
                ('ws_a', 'jrun-only-a'),
                ('ws_a', 'jrun-shared'),
                ('ws_b', 'jrun-shared');
            INSERT INTO invocations(ts, job_run_id, activity_id, agent) VALUES
                ('2026-10-04T00:00:00Z', 'jrun-only-a', 'implement_one', 'claude'),
                ('2026-10-04T00:00:00Z', 'jrun-shared', 'implement_one', 'claude'),
                ('2026-10-04T00:00:00Z', 'jrun-gone', 'implement_one', 'claude');
        "#,
    )
    .expect("seed legacy invocations");

    ledger::run_migrations(&conn, ledger::MIGRATIONS).expect("apply v36");

    let workspace_of = |run_id: &str| -> Option<String> {
        conn.query_row(
            "SELECT workspace_id FROM invocations WHERE job_run_id = ?1",
            [run_id],
            |row| row.get(0),
        )
        .expect("read invocation workspace")
    };
    assert_eq!(workspace_of("jrun-only-a").as_deref(), Some("ws_a"));
    assert_eq!(workspace_of("jrun-shared"), None);
    assert_eq!(workspace_of("jrun-gone"), None);
}
