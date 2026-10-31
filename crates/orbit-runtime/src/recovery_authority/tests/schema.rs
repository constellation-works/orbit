//! Persisted-authority safety: the public leaf API cannot seed host authority.

use orbit_store::contracts::{CompatibilityRecord, MigrationCompatibility};
use rusqlite::{Connection, params, types::Value};
use tempfile::TempDir;

use super::super::RecoveryAuthority;
use super::fixtures::{RUN_ID, STEP_ID, scope};

const TABLES: [&str; 5] = [
    "rebase_recovery_certificate",
    "rebase_recovery_attempt",
    "rebase_recovery_attempt_certificate",
    "worker_process_binding",
    "worker_namespace_binding",
];

const LEGACY_INSERTS: [&str; 5] = [
    "INSERT INTO rebase_recovery_certificate VALUES ('legacy','sync','/workspace','head','base','digest','issued')",
    "INSERT INTO rebase_recovery_attempt VALUES ('legacy','sync',1,'/workspace','head','base','reserved')",
    "INSERT INTO rebase_recovery_attempt_certificate VALUES ('legacy','sync',1,'/workspace','head','base','digest','issued')",
    "INSERT INTO worker_process_binding VALUES (42,'start','{}')",
    "INSERT INTO worker_namespace_binding VALUES ('namespace','{}')",
];

fn table_snapshot(connection: &Connection, table: &str) -> (String, Vec<Vec<Value>>) {
    let definition = connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name=?1",
            [table],
            |row| row.get(0),
        )
        .expect("read existing definition");
    let mut statement = connection
        .prepare(&format!("SELECT * FROM {table} ORDER BY 1,2"))
        .expect("scan rows");
    let columns = statement.column_count();
    let rows = statement
        .query_map([], |row| (0..columns).map(|index| row.get(index)).collect())
        .expect("query rows")
        .collect::<Result<_, _>>()
        .expect("read rows");
    (definition, rows)
}

#[test]
fn shipped_authority_shapes_adopt_without_reshaping_or_losing_old_writes() {
    if crate::tests::run_isolated_test(std::any::type_name_of_val(
        &shipped_authority_shapes_adopt_without_reshaping_or_losing_old_writes,
    )) {
        return;
    }
    let definitions: Vec<_> = include_str!("fixtures/authority_v0.sql")
        .split(';')
        .filter(|sql| !sql.trim().is_empty())
        .collect();
    assert_eq!(definitions.len(), TABLES.len());
    let reference = Connection::open_in_memory().expect("pre-ledger reference");
    reference
        .execute_batch(include_str!("fixtures/authority_v0.sql"))
        .expect("all shipped tables");
    // Before attempts: one certificate table. After attempts: three. Both
    // worker tables were independent first-use additions. Zero is a fresh file.
    for certificate_tables in [0, 1, 3] {
        for bindings in 0..4 {
            if certificate_tables == 0 && bindings != 0 {
                continue;
            }
            let global = TempDir::new().expect("global root");
            let root = global.path().join("state/recovery-authority");
            std::fs::create_dir_all(&root).expect("legacy root");
            let path = root.join("authority.db");
            let old = Connection::open(&path).expect("old binary open");
            let mut preserved = Vec::new();
            for index in 0..TABLES.len() {
                if index < certificate_tables || (index >= 3 && bindings & (1 << (index - 3)) != 0)
                {
                    old.execute_batch(definitions[index])
                        .expect("create shipped table");
                    old.execute_batch(LEGACY_INSERTS[index])
                        .expect("old binary write");
                    preserved.push((index, table_snapshot(&old, TABLES[index])));
                }
            }
            drop(old);
            #[cfg(unix)]
            assert!(
                super::super::worker::current_worker_binding(global.path())
                    .expect("read pre-ledger optional binding tables without migration")
                    .is_none()
            );
            let authority = RecoveryAuthority::open(global.path()).expect("adopt authority");
            for (index, before) in &preserved {
                assert_eq!(
                    &table_snapshot(&authority.connection, TABLES[*index]),
                    before,
                    "adoption preserves the pre-ledger table and all its rows"
                );
            }
            for table in TABLES {
                let (actual, _) = table_snapshot(&authority.connection, table);
                let (shipped, _) = table_snapshot(&reference, table);
                assert_eq!(
                    actual.split_whitespace().collect::<Vec<_>>(),
                    shipped.split_whitespace().collect::<Vec<_>>(),
                    "each v1 table preserves its shipped shape, including on a fresh database"
                );
            }
            let ledger = table_snapshot(&authority.connection, "schema_meta");
            let name: String = authority
                .connection
                .query_row(
                    "SELECT value FROM schema_meta WHERE key='migration.v0001'",
                    [],
                    |row| row.get(0),
                )
                .expect("v1 recorded");
            assert_eq!(name, "recovery_authority_baseline");
            let raw: String = authority
                .connection
                .query_row(
                    "SELECT value FROM schema_meta WHERE key='migration.compat'",
                    [],
                    |row| row.get(0),
                )
                .expect("compatibility recorded");
            let record = CompatibilityRecord::decode(&raw).expect("valid compatibility record");
            assert_eq!(record.version, 1);
            assert!(record.breaking.is_empty());
            assert_eq!(record.read_only, Some(vec![]));
            assert_eq!(
                authority
                    .begin_attempt(RUN_ID, STEP_ID, &scope(global.path()))
                    .expect("new writer"),
                1
            );
            drop(authority);
            // Re-run the old open/create statements and make old-shaped writes
            // after adoption; pre-ledger binaries know nothing of schema_meta.
            let old = Connection::open(&path).expect("older binary reopens");
            for (index, _) in &preserved {
                old.execute_batch(definitions[*index])
                    .expect("old open still works");
                old.execute(&format!("DELETE FROM {}", TABLES[*index]), [])
                    .expect("old delete");
                old.execute_batch(LEGACY_INSERTS[*index])
                    .expect("old write after adoption");
            }
            drop(old);
            let reopened = RecoveryAuthority::open(global.path()).expect("current binary reopens");
            assert_eq!(
                table_snapshot(&reopened.connection, "schema_meta"),
                ledger,
                "a current open does not rewrite ledger rows or their timestamps"
            );
            for (index, before) in &preserved {
                assert_eq!(
                    &table_snapshot(&reopened.connection, TABLES[*index]),
                    before
                );
            }
        }
    }
}

#[test]
fn newer_authority_schemas_keep_safe_writers_or_hold_reads_or_refuse() {
    if crate::tests::run_isolated_test(std::any::type_name_of_val(
        &newer_authority_schemas_keep_safe_writers_or_hold_reads_or_refuse,
    )) {
        return;
    }
    for (label, compatibility) in [
        ("additive", Some(MigrationCompatibility::Additive)),
        (
            "read-compatible",
            Some(MigrationCompatibility::ReadCompatible),
        ),
        ("breaking", Some(MigrationCompatibility::Breaking)),
        ("missing", None),
        ("corrupt", None),
        ("stale", None),
    ] {
        let global = TempDir::new().expect("global root");
        let authority = RecoveryAuthority::open(global.path()).expect("baseline authority");
        authority
            .connection
            .execute(
                "INSERT INTO schema_meta VALUES ('migration.v0002', 'future_authority', 'future')",
                [],
            )
            .expect("future ledger");
        if let Some(compatibility) = compatibility {
            let record = CompatibilityRecord::for_registry(
                2,
                [
                    (
                        1,
                        "recovery_authority_baseline",
                        MigrationCompatibility::Additive,
                    ),
                    (2, "future_authority", compatibility),
                ],
            );
            authority
                .connection
                .execute(
                    "UPDATE schema_meta SET value=?1 WHERE key='migration.compat'",
                    [record.encode().expect("encode future compatibility")],
                )
                .expect("future record");
        } else if label == "missing" {
            authority
                .connection
                .execute("DELETE FROM schema_meta WHERE key='migration.compat'", [])
                .expect("missing record");
        } else if label == "corrupt" {
            authority
                .connection
                .execute(
                    "UPDATE schema_meta SET value=?1 WHERE key='migration.compat'",
                    params!["not-json"],
                )
                .expect("corrupt record");
        }
        let ledger = table_snapshot(&authority.connection, "schema_meta");
        drop(authority);
        let opened = RecoveryAuthority::open(global.path());
        if matches!(
            compatibility,
            Some(MigrationCompatibility::Additive | MigrationCompatibility::ReadCompatible)
        ) {
            let authority = opened.expect("safe reader opens");
            let query_only: bool = authority
                .connection
                .pragma_query_value(None, "query_only", |row| row.get(0))
                .expect("writer mode");
            assert_eq!(
                query_only,
                compatibility == Some(MigrationCompatibility::ReadCompatible)
            );
            let write = authority.begin_attempt(RUN_ID, STEP_ID, &scope(global.path()));
            if query_only {
                assert!(
                    write
                        .expect_err("older writes refused")
                        .to_string()
                        .contains("readonly")
                );
            } else {
                assert_eq!(write.expect("safe older writer"), 1);
            }
        } else {
            let error = opened.expect_err("unsafe authority refuses open");
            assert!(
                error
                    .to_string()
                    .contains("recovery authority database schema version 2"),
                "{label}: {error}"
            );
        }
        let root = global.path().join("state/recovery-authority");
        for sidecar in ["authority.db-wal", "authority.db-shm"] {
            assert!(
                root.join(sidecar).is_file(),
                "{label} must keep {sidecar} for workers without authority write access"
            );
        }
        #[cfg(unix)]
        {
            let lookup = super::super::worker::current_worker_binding(global.path());
            if matches!(
                compatibility,
                Some(MigrationCompatibility::Additive | MigrationCompatibility::ReadCompatible)
            ) {
                assert!(lookup.expect("safe worker read").is_none());
            } else {
                assert!(
                    lookup
                        .expect_err("unsafe worker read refused")
                        .to_string()
                        .contains("schema version 2")
                );
            }
        }
        let connection =
            Connection::open(global.path().join("state/recovery-authority/authority.db"))
                .expect("inspect authority");
        assert_eq!(
            table_snapshot(&connection, "schema_meta"),
            ledger,
            "older opens never migrate newer authority"
        );
    }
}
