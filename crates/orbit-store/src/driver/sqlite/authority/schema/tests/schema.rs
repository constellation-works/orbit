//! Recovery authority schema adoption and forward-compatibility tests.

use std::path::Path;

use rusqlite::types::Value;
use rusqlite::{Connection, params};
use tempfile::TempDir;

use crate::contract::{CompatibilityRecord, MigrationCompatibility};
use crate::driver::sqlite::authority::FILE_NAME;
use crate::repository::recovery_authority::{
    RecoveryAttemptRecord, RecoveryAuthorityReadOnly, RecoveryAuthorityStore,
};

const RUN_ID: &str = "jrun-20260910-0001-1";

const STEP_ID: &str = "sync_base";

const HEAD_BEFORE: &str = "1111111111111111111111111111111111111111";

const PINNED_BASE: &str = "3333333333333333333333333333333333333333";

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

fn reserve_attempt(
    store: &RecoveryAuthorityStore,
    workspace: &Path,
) -> Result<u64, orbit_common::OrbitError> {
    let record = RecoveryAttemptRecord {
        workspace_path: workspace.to_string_lossy().into_owned(),
        head_sha_before: HEAD_BEFORE.to_string(),
        target_base_sha: PINNED_BASE.to_string(),
    };
    store.write("reserve rebase recovery attempt", |tx| {
        let attempt = tx.latest_reservation(RUN_ID, STEP_ID)?.unwrap_or(0) + 1;
        tx.insert_reservation(RUN_ID, STEP_ID, attempt, &record, "2026-11-25T00:00:00Z")?;
        u64::try_from(attempt).map_err(|_| {
            orbit_common::OrbitError::Execution("rebase recovery attempt is out of range".into())
        })
    })
}

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
            let path = root.join(FILE_NAME);
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
            {
                let reader = RecoveryAuthorityReadOnly::open(&path)
                    .expect("read pre-ledger optional binding tables without migration");
                if reader
                    .has_worker_namespace_table()
                    .expect("check namespace table")
                {
                    assert!(
                        reader
                            .worker_namespace_binding("unbound")
                            .expect("read namespace")
                            .is_none()
                    );
                }
                if reader
                    .has_worker_process_table()
                    .expect("check process table")
                {
                    assert!(
                        reader
                            .worker_process_binding(999_999, "missing")
                            .expect("read process")
                            .is_none()
                    );
                }
            }
            let authority = RecoveryAuthorityStore::open(&path).expect("adopt authority");
            for (index, before) in &preserved {
                assert_eq!(
                    &table_snapshot(authority.connection(), TABLES[*index]),
                    before,
                    "adoption preserves the pre-ledger table and all its rows"
                );
            }
            for table in TABLES {
                let (actual, _) = table_snapshot(authority.connection(), table);
                let (shipped, _) = table_snapshot(&reference, table);
                assert_eq!(
                    actual.split_whitespace().collect::<Vec<_>>(),
                    shipped.split_whitespace().collect::<Vec<_>>(),
                    "each v1 table preserves its shipped shape, including on a fresh database"
                );
            }
            let ledger = table_snapshot(authority.connection(), "schema_meta");
            let name: String = authority
                .connection()
                .query_row(
                    "SELECT value FROM schema_meta WHERE key='migration.v0001'",
                    [],
                    |row| row.get(0),
                )
                .expect("v1 recorded");
            assert_eq!(name, "recovery_authority_baseline");
            let raw: String = authority
                .connection()
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
                reserve_attempt(&authority, global.path()).expect("new writer"),
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
            let reopened = RecoveryAuthorityStore::open(&path).expect("current binary reopens");
            assert_eq!(
                table_snapshot(reopened.connection(), "schema_meta"),
                ledger,
                "a current open does not rewrite ledger rows or their timestamps"
            );
            for (index, before) in &preserved {
                assert_eq!(
                    &table_snapshot(reopened.connection(), TABLES[*index]),
                    before
                );
            }
        }
    }
}

#[test]
fn newer_authority_schemas_keep_safe_writers_or_hold_reads_or_refuse() {
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
        let root = global.path().join("state/recovery-authority");
        std::fs::create_dir_all(&root).expect("authority root");
        let path = root.join(FILE_NAME);
        let authority = RecoveryAuthorityStore::open(&path).expect("baseline authority");
        authority
            .connection()
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
                .connection()
                .execute(
                    "UPDATE schema_meta SET value=?1 WHERE key='migration.compat'",
                    [record.encode().expect("encode future compatibility")],
                )
                .expect("future record");
        } else if label == "missing" {
            authority
                .connection()
                .execute("DELETE FROM schema_meta WHERE key='migration.compat'", [])
                .expect("missing record");
        } else if label == "corrupt" {
            authority
                .connection()
                .execute(
                    "UPDATE schema_meta SET value=?1 WHERE key='migration.compat'",
                    params!["not-json"],
                )
                .expect("corrupt record");
        }
        let ledger = table_snapshot(authority.connection(), "schema_meta");
        drop(authority);
        let opened = RecoveryAuthorityStore::open(&path);
        if matches!(
            compatibility,
            Some(MigrationCompatibility::Additive | MigrationCompatibility::ReadCompatible)
        ) {
            let authority = opened.expect("safe reader opens");
            let query_only: bool = authority
                .connection()
                .pragma_query_value(None, "query_only", |row| row.get(0))
                .expect("writer mode");
            assert_eq!(
                query_only,
                compatibility == Some(MigrationCompatibility::ReadCompatible)
            );
            let write = reserve_attempt(&authority, global.path());
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
        for sidecar in ["authority.db-wal", "authority.db-shm"] {
            assert!(
                root.join(sidecar).is_file(),
                "{label} must keep {sidecar} for workers without authority write access"
            );
        }
        #[cfg(unix)]
        {
            let lookup = RecoveryAuthorityReadOnly::open(&path);
            if matches!(
                compatibility,
                Some(MigrationCompatibility::Additive | MigrationCompatibility::ReadCompatible)
            ) {
                assert!(lookup.is_ok(), "safe worker read");
            } else {
                assert!(
                    lookup
                        .expect_err("unsafe worker read refused")
                        .to_string()
                        .contains("schema version 2")
                );
            }
        }
        let connection = Connection::open(&path).expect("inspect authority");
        assert_eq!(
            table_snapshot(&connection, "schema_meta"),
            ledger,
            "older opens never migrate newer authority"
        );
    }
}
