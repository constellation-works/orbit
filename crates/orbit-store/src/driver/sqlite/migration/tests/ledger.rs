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

/// A database that recorded shipped v1 and then advances through every
/// registered migration must expose the same table/column structure as a
/// database created fresh by the current binary.
#[test]
fn v1_upgrade_and_fresh_database_have_identical_columns() {
    let fresh = Connection::open_in_memory().expect("open fresh database");
    apply_schema(&fresh).expect("migrate fresh database");

    let legacy = Connection::open_in_memory().expect("open legacy database");
    ledger::run_migrations(&legacy, &ledger::MIGRATIONS[..1]).expect("record shipped v1");
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
