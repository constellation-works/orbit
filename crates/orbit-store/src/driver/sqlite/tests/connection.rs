//! Sibling tests for `sqlite/connection.rs` health probes [ORB-10005].

use orbit_common::OrbitError;

use crate::Store;
use crate::driver::sqlite::migration::SUPPORTED_SCHEMA_VERSION;

#[test]
fn quick_check_passes_on_fresh_store() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(&dir.path().join("store.db")).expect("open store");
    store.quick_check().expect("fresh store passes quick_check");
}

#[test]
fn quick_check_passes_in_memory() {
    let store = Store::open_in_memory().expect("open in-memory store");
    store
        .quick_check()
        .expect("in-memory store passes quick_check");
}

#[test]
fn check_writable_acquires_and_releases_write_lock() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(&dir.path().join("store.db")).expect("open store");

    store.check_writable().expect("writable store passes");
    // The probe rolled back: the write lock is free again immediately.
    store.check_writable().expect("probe is repeatable");
    // And the store still accepts real transactions afterwards.
    store
        .with_transaction(|_| Ok(()))
        .expect("store still accepts transactions after the probe");
}

#[test]
fn path_write_probe_does_not_create_or_migrate_databases() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("probe.db");
    assert!(Store::check_path_writable(&path).is_err());
    assert!(!path.exists(), "probe must not create a missing database");

    let conn = rusqlite::Connection::open(&path).expect("create plain database");
    conn.execute_batch("CREATE TABLE sentinel(value INTEGER); INSERT INTO sentinel VALUES (7);")
        .expect("create existing data");
    Store::check_path_writable(&path).expect("existing writable database passes");
    let tables: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table'",
            [],
            |row| row.get(0),
        )
        .expect("count tables after probe");
    assert_eq!(tables, 1, "probe must not apply Orbit migrations");
    let value: i64 = conn
        .query_row("SELECT value FROM sentinel", [], |row| row.get(0))
        .expect("read unchanged data");
    assert_eq!(value, 7);

    conn.execute_batch("BEGIN IMMEDIATE")
        .expect("hold write lock");
    assert!(
        Store::check_path_writable(&path).is_err(),
        "probe must acquire actual write access"
    );
    conn.execute_batch("ROLLBACK").expect("release write lock");
    Store::check_path_writable(&path).expect("probe succeeds after write lock release");
}

#[test]
fn transaction_and_read_callbacks_expose_scoped_sql_connections() {
    let store = Store::open_in_memory().expect("open in-memory store");
    store
        .with_transaction(|tx| {
            tx.connection()
                .execute_batch(
                    "CREATE TABLE feature_callback_fixture(value TEXT NOT NULL);
                     INSERT INTO feature_callback_fixture VALUES ('committed');",
                )
                .map_err(|error| OrbitError::Store(error.to_string()))
        })
        .expect("commit feature-owned SQL");

    let value = store
        .with_read_connection(|conn| {
            conn.query_row("SELECT value FROM feature_callback_fixture", [], |row| {
                row.get::<_, String>(0)
            })
            .map_err(|error| OrbitError::Store(error.to_string()))
        })
        .expect("read through callback");
    assert_eq!(value, "committed");
}

#[test]
fn file_backed_read_callback_is_query_only() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(&dir.path().join("store.db")).expect("open store");

    store
        .with_read_connection(|conn| {
            let write = conn.execute_batch("CREATE TABLE read_callback_write(value TEXT)");
            assert!(write.is_err(), "pooled read connection must reject writes");
            Ok(())
        })
        .expect("inspect query-only connection");

    let table_count = store
        .with_read_connection(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'table' AND name = 'read_callback_write'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| OrbitError::Store(error.to_string()))
        })
        .expect("verify rejected write");
    assert_eq!(table_count, 0);
}

#[test]
fn quick_check_reports_page_corruption() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("store.db");

    // Build a multi-page database, then close every connection.
    {
        let store = Store::open(&path).expect("open store");
        store
            .with_transaction(|tx| {
                tx.tx
                    .execute_batch("CREATE TABLE corruption_fixture(payload TEXT)")
                    .map_err(|e| orbit_common::OrbitError::Store(e.to_string()))?;
                for _ in 0..64 {
                    tx.tx
                        .execute(
                            "INSERT INTO corruption_fixture VALUES (hex(randomblob(512)))",
                            [],
                        )
                        .map_err(|e| orbit_common::OrbitError::Store(e.to_string()))?;
                }
                Ok(())
            })
            .expect("seed fixture rows");
    }
    // Checkpoint the WAL into the main file so on-disk bytes are canonical.
    {
        let conn = rusqlite::Connection::open(&path).expect("open raw");
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .expect("checkpoint");
    }

    // Clobber the header of the last b-tree page (page 1 — the file header
    // and sqlite_master — stays intact so the database still opens). The
    // page header carries the page type and cell pointers, so structural
    // validation must notice; flipping payload bytes alone would not.
    let mut bytes = std::fs::read(&path).expect("read db bytes");
    let page_size = usize::from(u16::from_be_bytes([bytes[16], bytes[17]]));
    assert!(
        bytes.len() >= page_size * 3,
        "fixture must span multiple pages (len {}, page size {page_size})",
        bytes.len()
    );
    let last_page_start = (bytes.len() / page_size - 1) * page_size;
    for byte in &mut bytes[last_page_start..last_page_start + 64] {
        *byte ^= 0xFF;
    }
    std::fs::write(&path, bytes).expect("write corrupted db");

    let store = Store::open(&path).expect("corrupted db still opens");
    let err = store
        .quick_check()
        .expect_err("quick_check must flag the corrupted page");
    assert!(
        err.to_string().contains("quick_check"),
        "error names the failing probe: {err}"
    );
}

#[test]
fn schema_version_matches_binary_after_open() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(&dir.path().join("store.db")).expect("open store");
    assert_eq!(
        store.schema_version().expect("schema version"),
        SUPPORTED_SCHEMA_VERSION,
        "a freshly-opened store is migrated to the binary's schema version"
    );
}

#[test]
fn current_store_bootstrap_names_locked_write_admission_after_busy_timeout() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("store.db");
    drop(Store::open(&path).expect("initialize current store"));

    let blocker = rusqlite::Connection::open(&path).expect("open blocker");
    blocker
        .execute_batch("BEGIN EXCLUSIVE")
        .expect("hold SQLite write lock");
    let started = std::time::Instant::now();
    let error = match Store::open(&path) {
        Ok(_) => panic!("bootstrap must exhaust the busy timeout"),
        Err(error) => error,
    };

    assert!(
        started.elapsed()
            >= std::time::Duration::from_millis(u64::from(
                orbit_common::storage::sqlite::DEFAULT_BUSY_TIMEOUT_MS,
            )),
        "the regression must exercise SQLite's real busy timeout"
    );
    let contention = error
        .sqlite_contention()
        .expect("lock failure remains typed across the store boundary");
    assert_eq!(contention.path, path.display().to_string());
    assert_eq!(contention.phase, "bootstrap write admission");
}

#[cfg(unix)]
#[test]
fn file_store_is_private_under_permissive_umask() {
    use std::os::unix::fs::PermissionsExt;

    const CHILD_MARKER: &str = "ORBIT_TEST_PRIVATE_MAIN_SQLITE";
    if std::env::var_os(CHILD_MARKER).is_none() {
        let status = std::process::Command::new("sh")
            .args(["-c", "umask 000; exec \"$@\"", "sh"])
            .arg(std::env::current_exe().expect("current test executable"))
            .arg("file_store_is_private_under_permissive_umask")
            .env(CHILD_MARKER, "1")
            .status()
            .expect("run test under permissive umask");
        assert!(status.success(), "permissive-umask child failed");
        return;
    }

    let root = tempfile::tempdir().expect("tempdir");
    let state_dir = root.path().join("private/state");
    let path = state_dir.join("orbit.db");
    let store = Store::open(&path).expect("open store");
    store
        .with_transaction(|tx| {
            tx.connection()
                .execute_batch(
                    "CREATE TABLE permission_fixture(value TEXT NOT NULL);
                     INSERT INTO permission_fixture VALUES ('secret');",
                )
                .map_err(|error| OrbitError::Store(error.to_string()))
        })
        .expect("persist fixture");

    for directory in [state_dir.parent().expect("private parent"), &state_dir] {
        let mode = std::fs::metadata(directory)
            .expect("directory metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700, "private directory {}", directory.display());
    }
    for suffix in ["", "-wal", "-shm"] {
        let file = std::path::PathBuf::from(format!("{}{suffix}", path.display()));
        let mode = std::fs::metadata(&file)
            .expect("SQLite file metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "private SQLite file {}", file.display());
    }
}

#[test]
fn read_only_probe_does_not_create_a_missing_database() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("missing.db");
    assert!(Store::open_read_only(&path).is_err());
    assert!(!path.exists(), "diagnosis must not create the database");
}

#[test]
fn read_only_probe_preserves_unmigrated_database_and_rejects_writes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("store.db");
    let connection = rusqlite::Connection::open(&path).expect("fixture database");
    connection
        .execute_batch("CREATE TABLE fixture (id INTEGER)")
        .expect("fixture table");
    drop(connection);
    let before = std::fs::read(&path).expect("database before probe");

    let probe = Store::open_read_only(&path).expect("read-only probe");
    probe.quick_check().expect("valid database");
    assert_eq!(probe.schema_version().expect("version"), 0);
    assert!(
        probe
            .with_transaction(|tx| {
                tx.connection()
                    .execute("INSERT INTO fixture VALUES (1)", [])
                    .map_err(|error| OrbitError::Store(error.to_string()))?;
                Ok(())
            })
            .is_err(),
        "probe must reject writes"
    );
    drop(probe);
    assert_eq!(std::fs::read(&path).expect("database after probe"), before);
}

#[test]
fn read_only_probe_reports_malformed_database_without_repair() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("store.db");
    let content = b"not a SQLite database";
    std::fs::write(&path, content).expect("malformed fixture");
    let probe = Store::open_read_only(&path).expect("open existing file");
    assert!(probe.quick_check().is_err());
    assert_eq!(std::fs::read(&path).expect("unchanged file"), content);
}
