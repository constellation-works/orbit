// ORB-10003: versioned schema-migration ledger.
use std::path::Path;

use orbit_common::OrbitError;
use orbit_common::storage::sqlite::sqlite_store_error;

use crate::contracts::MigrationCompatibility;
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

fn migration_v1_marker(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch("CREATE TABLE ledger_test_v1 (x INTEGER)")
        .map_err(sqlite_store_error)
}

fn migration_v2_fails_midway(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch(
        "CREATE TABLE ledger_test_half_applied (x INTEGER);\n         this is not valid migration SQL",
    )
        .map_err(sqlite_store_error)?;
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

fn migration_v1_marker_v2(conn: &Connection) -> Result<(), OrbitError> {
    conn.execute_batch("CREATE TABLE ledger_test_v2 (x INTEGER)")
        .map_err(sqlite_store_error)
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

/// The two resource-minute tables, and the admission reservations and
/// forecasts after them, are additive: a binary without them keeps opening the migrated database
/// read-write, and upgrade admission sees no new writer or reader floor, so
/// they force no generation switch while old drains still run.
#[test]
fn resource_minute_tables_keep_an_older_binary_writing() {
    let position = |name: &str| {
        ledger::MIGRATIONS
            .iter()
            .position(|migration| migration.name == name)
            .expect("registered migration")
    };
    let first = position("run_resource_minutes");
    let last = position("worker_cpu_accounting");
    let newer = &ledger::MIGRATIONS[..=last];
    let older = &ledger::MIGRATIONS[..first];

    let conn = Connection::open_in_memory().expect("open in-memory connection");
    ledger::run_migrations(&conn, &newer[..last]).expect("pre-provenance schema");
    let legacy_insert = "INSERT INTO run_resource_minutes (
        workspace_id, run_id, minute_index, minute_started_at, task_id,
        work_class, cpu_cores, memory_bytes, samples
    ) VALUES ('ws_a', ?1, 0, '2026-10-13T00:00:00Z', 'task', 'change', 0.03, 4096, 12)";
    conn.execute(legacy_insert, ["before"]).expect("old sample");
    assert!(
        ledger::run_migrations(&conn, newer)
            .expect("migrate")
            .is_none()
    );
    for table in [
        "run_resource_minutes",
        "host_resource_minutes",
        "admission_reservations",
        "admission_forecasts",
    ] {
        assert!(table_exists(&conn, table).expect("introspect"), "{table}");
    }
    let open = ledger::run_migrations(&conn, older)
        .expect("an older binary opens the database")
        .expect("the database is newer than that binary");
    assert!(open.writable, "{open}");

    let floors = |registry: &[Migration]| {
        let compatibility = orbit_common::fs::generation::LedgerCompatibility::from_registry(
            registry
                .iter()
                .map(|m| (m.version, m.compat.is_write_safe(), !m.compat.is_breaking())),
        );
        (compatibility.writer_floor, compatibility.reader_floor)
    };
    assert_eq!(floors(newer), floors(older));
    conn.execute(legacy_insert, ["after"])
        .expect("an old writer can still insert");
    let mut statement = conn
        .prepare("SELECT cpu_cores, memory_bytes, cpu_accounting FROM run_resource_minutes")
        .expect("raw historical evidence");
    let rows: Vec<(f64, u64, Option<String>)> = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .expect("read minutes")
        .collect::<Result<_, _>>()
        .expect("raw rows");
    assert_eq!(rows, vec![(0.03, 4096, None); 2]);
}

// Safety invariant: every shipped ledger prefix has a stable schema. The
// public open boundary always runs the latest registry, so it cannot expose
// historical prefixes without widening production visibility (criterion 3).
#[test]
fn store_schema_prefix_goldens() {
    check_schema_prefix_goldens(
        "driver::sqlite::migration::tests::ledger::store_schema_prefix_goldens",
        &ledger::STORE_DATABASE,
        ledger::MIGRATIONS,
        "src/driver/sqlite/migration/tests/store_schema.golden",
    );
}

// The same safety invariant for the task registry's own ledger, a separate
// SQLite file with a separate registry (criterion 3).
#[test]
fn task_registry_schema_prefix_goldens() {
    check_schema_prefix_goldens(
        "driver::sqlite::migration::tests::ledger::task_registry_schema_prefix_goldens",
        &crate::driver::sqlite::task_registry::ledger::REGISTRY_DATABASE,
        crate::driver::sqlite::task_registry::ledger::MIGRATIONS,
        "src/driver/sqlite/task_registry/tests/registry_schema.golden",
    );
}

fn check_schema_prefix_goldens(
    test: &str,
    database: &ledger::LedgeredDatabase,
    migrations: &[Migration],
    golden: &str,
) {
    const CHILD_ENV: &str = "ORBIT_STORE_SCHEMA_GOLDEN_CHILD";
    if std::env::var_os(CHILD_ENV).is_none() {
        let mut command = std::process::Command::new(std::env::current_exe().expect("test binary"));
        let root = tempfile::tempdir_in(orbit_common::test_env::canonical_temp_dir())
            .expect("isolated golden root");
        orbit_common::test_env::clear_inherited_authority(|key| {
            command.env_remove(key);
        });
        command
            .args(["--exact", test, "--nocapture"])
            .env(CHILD_ENV, "1")
            .env("HOME", root.path())
            .env("USERPROFILE", root.path())
            .current_dir(root.path());
        let output = orbit_common::test_env::run_child_test(&mut command, test, root.path());
        orbit_common::test_env::assert_child_test_passed(
            test,
            output.status,
            output.stdout,
            output.stderr,
        );
        return;
    }

    let mut previous = std::collections::BTreeSet::new();
    let mut actual = String::new();
    // Build each prefix independently: its compatibility record and schema
    // are exactly those of a binary whose registry ended at this version.
    for end in 1..=migrations.len() {
        let conn = Connection::open_in_memory().expect("schema fixture");
        ledger::run_database_migrations(&conn, database, &migrations[..end], None)
            .expect("apply ledger prefix");
        let mut stmt = conn
            .prepare(
                "SELECT type, name, tbl_name, sql FROM sqlite_schema ORDER BY type, name, tbl_name",
            )
            .expect("schema dump");
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })
            .expect("schema rows");
        let current: std::collections::BTreeSet<String> = rows
            .map(|row| {
                let (kind, name, table, sql) = row.expect("schema row");
                // Normalize outer layout only; retain every byte inside SQL so
                // whitespace within string literals stays significant. Keep
                // NULL SQL on automatic indexes; omit physical root pages.
                let sql = sql.map(|sql| sql.trim().to_owned());
                serde_json::to_string(&(kind, name, table, sql)).expect("serialize schema row")
            })
            .collect();
        let migration = &migrations[end - 1];
        actual.push_str(&format!("v{} {}\n", migration.version, migration.name));
        // Lossless deltas keep all cumulative dumps reviewable: replaying
        // these removals/additions reconstructs the full dump at every N.
        for removed in previous.difference(&current) {
            actual.push_str(&format!("- {removed}\n"));
        }
        for added in current.difference(&previous) {
            actual.push_str(&format!("+ {added}\n"));
        }
        actual.push('\n');
        previous = current;
    }
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(golden);
    if std::env::var_os("ORBIT_UPDATE_STORE_SCHEMA_GOLDENS").is_some() {
        std::fs::write(&path, &actual).expect("write schema golden");
    }
    let expected = std::fs::read_to_string(&path).expect("read schema golden");
    assert_eq!(
        actual, expected,
        "shipped {} schema changed; append a migration instead of editing a shipped body; \
         regenerate intentional additions with make goldens UPDATE=1",
        database.noun
    );
}
