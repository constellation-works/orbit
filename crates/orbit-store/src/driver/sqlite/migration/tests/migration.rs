// Migrated from sqlite/migration.rs per ORB-00231
use super::super::*;

// ── read-only ledger inspection for `orbit migrate --dry-run` [ORB-10012] ──

#[test]
fn read_schema_ledger_status_of_a_missing_database_lists_everything_pending() {
    let temp = tempfile::tempdir().expect("tempdir");
    let status =
        read_schema_ledger_status(&temp.path().join("orbit.db")).expect("read missing db status");

    assert_eq!(status.current_version, 0);
    assert!(!status.pending.is_empty());
    assert_eq!(
        status.pending.last().map(|m| m.version),
        Some(SUPPORTED_SCHEMA_VERSION)
    );
    // Read-only: inspecting must not create the database.
    assert!(!temp.path().join("orbit.db").exists());
}

#[test]
fn read_schema_ledger_status_of_a_migrated_database_reports_current_without_writing() {
    let temp = tempfile::tempdir().expect("tempdir");
    let db_path = temp.path().join("orbit.db");
    drop(crate::Store::open(&db_path).expect("open store"));

    let status = read_schema_ledger_status(&db_path).expect("read status");
    assert_eq!(status.current_version, SUPPORTED_SCHEMA_VERSION);
    assert!(status.pending.is_empty());
    assert!(pending_schema_migrations_after(SUPPORTED_SCHEMA_VERSION).is_empty());
    assert_eq!(
        pending_schema_migrations_after(0).last().map(|m| m.version),
        Some(SUPPORTED_SCHEMA_VERSION)
    );
}
