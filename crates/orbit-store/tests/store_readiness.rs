//! Worker store readiness through public composition, separate from identity
//! allocation and coordinated admission. Mutable fixtures run in isolated
//! children with cleared Orbit authority and a disposable home.

#![allow(clippy::expect_used, clippy::unwrap_used, missing_docs)]

use std::path::Path;
use std::process::Command;

use orbit_common::{OrbitError, process, test_env};
use orbit_store::Store;
use orbit_store::compose::ensure_sqlite_store_ready;
use orbit_store::contracts::{CompatibilityRecord, MigrationCompatibility};
use orbit_store::maintenance::migration::{SUPPORTED_SCHEMA_VERSION, read_schema_ledger_status};
use rusqlite::Connection;
use tempfile::TempDir;

fn isolated(test: &str) -> bool {
    const MARKER: &str = "ORBIT_TEST_STORE_READINESS_CHILD";
    if std::env::var(MARKER).as_deref() == Ok(test) {
        return true;
    }
    let home = TempDir::new().unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    command
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env(MARKER, test)
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .current_dir(home.path());
    let output =
        process::run_bounded_capped(&mut command, test_env::CHILD_TEST_DEADLINE, 256 * 1024)
            .expect("run isolated readiness fixture");
    test_env::assert_child_test_passed(test, output.status, output.stdout, output.stderr);
    false
}

fn supported_database(root: &Path) -> std::path::PathBuf {
    let database = root.join("store.sqlite");
    drop(Store::open(&database).expect("create supported-schema store"));
    assert_eq!(
        read_schema_ledger_status(&database)
            .unwrap()
            .current_version,
        SUPPORTED_SCHEMA_VERSION
    );
    database
}

#[test]
fn writable_supported_store_is_ready_without_changing_state() {
    if !isolated("writable_supported_store_is_ready_without_changing_state") {
        return;
    }
    let root = TempDir::new().unwrap();
    let database = supported_database(root.path());
    let before = std::fs::read(&database).unwrap();

    ensure_sqlite_store_ready(&database).expect("writable store is ready");

    assert_eq!(std::fs::read(&database).unwrap(), before);
    let conn = Connection::open(&database).unwrap();
    conn.execute_batch("BEGIN IMMEDIATE; ROLLBACK;")
        .expect("readiness releases its write lock");
}

#[test]
fn observationally_read_only_supported_store_is_not_ready() {
    if !isolated("observationally_read_only_supported_store_is_not_ready") {
        return;
    }
    let root = TempDir::new().unwrap();
    let database = supported_database(root.path());
    let original = std::fs::metadata(&database).unwrap().permissions();
    let mut read_only = original.clone();
    read_only.set_readonly(true);
    std::fs::set_permissions(&database, read_only).unwrap();
    let before = std::fs::read(&database).unwrap();

    let store = Store::open(&database).expect("supported schema remains readable");
    assert!(store.is_read_only(), "fixture must open observationally");
    assert!(store.forward_compatible_open().is_none());
    drop(store);
    let readiness = ensure_sqlite_store_ready(&database);

    // Restore before asserting so the disposable fixture can be cleaned on
    // platforms that refuse to remove read-only files.
    std::fs::set_permissions(&database, original).unwrap();
    let error = readiness.expect_err("workers must refuse an observational store pre-claim");
    assert!(matches!(error, OrbitError::Store(_)), "{error}");
    assert_eq!(std::fs::read(&database).unwrap(), before);
}

/// A read-only directory refuses the WAL sidecars, so SQLite reports the
/// denial natively; the translator, not the message text, must classify it so
/// the store degrades to observational reads instead of failing the open.
#[cfg(unix)]
#[test]
fn store_in_read_only_directory_degrades_to_observational_reads() {
    use std::os::unix::fs::PermissionsExt;

    if !isolated("store_in_read_only_directory_degrades_to_observational_reads") {
        return;
    }
    let root = TempDir::new().unwrap();
    let directory = root.path().join("state");
    std::fs::create_dir(&directory).unwrap();
    let database = supported_database(&directory);
    // Leave a cleanly closed file set with no sidecars, so any write needs a
    // new file in the directory.
    Connection::open(&database)
        .unwrap()
        .pragma_update(None, "journal_mode", "DELETE")
        .unwrap();
    let before = std::fs::read(&database).unwrap();
    let original = std::fs::metadata(&directory).unwrap().permissions();
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o555)).unwrap();
    if std::fs::write(directory.join("probe"), b"").is_ok() {
        // Privileged users bypass directory modes; the fixture cannot deny.
        std::fs::set_permissions(&directory, original).unwrap();
        return;
    }

    let opened = Store::open(&database);
    let readiness = ensure_sqlite_store_ready(&database);

    std::fs::set_permissions(&directory, original).unwrap();
    let store = opened.expect("a denied directory degrades instead of failing the open");
    assert!(store.is_read_only(), "the store must open observationally");
    drop(store);
    let error = readiness.expect_err("workers must refuse an observational store pre-claim");
    assert!(matches!(error, OrbitError::Store(_)), "{error}");
    assert_eq!(std::fs::read(&database).unwrap(), before);
}

#[test]
fn forward_compatible_read_only_store_keeps_migration_refusal() {
    if !isolated("forward_compatible_read_only_store_keeps_migration_refusal") {
        return;
    }
    let root = TempDir::new().unwrap();
    let database = supported_database(root.path());
    let newer = SUPPORTED_SCHEMA_VERSION + 1;
    let record = CompatibilityRecord::for_registry(
        newer,
        [(
            newer,
            "newer_read_only_fixture",
            MigrationCompatibility::ReadCompatible,
        )],
    );
    let conn = Connection::open(&database).unwrap();
    conn.execute(
        "INSERT INTO schema_meta(key, value, updated_at) VALUES (?1, ?2, 'fixture')",
        rusqlite::params![format!("migration.v{newer:04}"), "newer_read_only_fixture"],
    )
    .unwrap();
    conn.execute(
        "UPDATE schema_meta SET value = ?1 WHERE key = 'migration.compat'",
        [serde_json::to_string(&record).unwrap()],
    )
    .unwrap();
    drop(conn);
    let before = std::fs::read(&database).unwrap();
    let store = Store::open(&database).expect("newer schema remains readable");
    let forward = store.forward_compatible_open().expect("newer schema");
    assert_eq!(forward.state_version, newer);
    assert!(!forward.writable);
    drop(store);

    let error = ensure_sqlite_store_ready(&database)
        .expect_err("workers must refuse a newer read-only schema");

    assert!(matches!(error, OrbitError::Migration(_)), "{error}");
    assert_eq!(std::fs::read(&database).unwrap(), before);
}
