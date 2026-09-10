use rusqlite::Connection;

use super::super::sqlite::{DEFAULT_BUSY_TIMEOUT_MS, apply_default_pragmas};

fn pragma_i64(conn: &Connection, name: &str) -> i64 {
    conn.pragma_query_value(None, name, |row| row.get::<_, i64>(0))
        .expect("query pragma")
}

fn pragma_string(conn: &Connection, name: &str) -> String {
    conn.pragma_query_value(None, name, |row| row.get::<_, String>(0))
        .expect("query pragma")
}

#[test]
fn file_backed_connection_gets_all_defaults() {
    let dir = tempfile::tempdir().expect("tempdir");
    let conn = Connection::open(dir.path().join("defaults.db")).expect("open");

    let outcome = apply_default_pragmas(&conn).expect("apply defaults");

    assert!(
        outcome.wal_active(),
        "journal_mode: {}",
        outcome.journal_mode
    );
    assert_eq!(pragma_string(&conn, "journal_mode").to_lowercase(), "wal");
    assert_eq!(
        pragma_i64(&conn, "busy_timeout"),
        i64::from(DEFAULT_BUSY_TIMEOUT_MS)
    );
    assert_eq!(pragma_i64(&conn, "foreign_keys"), 1);
    // synchronous=NORMAL is reported as 1.
    assert_eq!(pragma_i64(&conn, "synchronous"), 1);
}

#[test]
fn in_memory_connection_keeps_memory_journal_without_error() {
    let conn = Connection::open_in_memory().expect("open in-memory");

    let outcome = apply_default_pragmas(&conn).expect("apply defaults");

    assert!(!outcome.wal_active());
    assert_eq!(outcome.journal_mode.to_lowercase(), "memory");
    assert_eq!(pragma_i64(&conn, "foreign_keys"), 1);
    assert_eq!(
        pragma_i64(&conn, "busy_timeout"),
        i64::from(DEFAULT_BUSY_TIMEOUT_MS)
    );
}

#[test]
fn defaults_are_idempotent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let conn = Connection::open(dir.path().join("idempotent.db")).expect("open");

    apply_default_pragmas(&conn).expect("first apply");
    let outcome = apply_default_pragmas(&conn).expect("second apply");

    assert!(outcome.wal_active());
}

#[test]
fn private_open_rejects_parent_directory_traversal() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("nested").join("..").join("escape.db");

    let error = match super::super::sqlite::open_private(&path) {
        Ok(_) => panic!("reject traversal"),
        Err(error) => error,
    };

    assert!(error.to_string().contains("parent-directory traversal"));
    assert!(!dir.path().join("nested").exists());
    assert!(!dir.path().join("escape.db").exists());
}

#[cfg(unix)]
#[test]
fn private_open_rejects_symlink_database() {
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("target.db");
    Connection::open(&target).expect("create target");
    let link = dir.path().join("link.db");
    std::os::unix::fs::symlink(&target, &link).expect("create symlink");

    let error = match super::super::sqlite::open_private(&link) {
        Ok(_) => panic!("reject symlink database"),
        Err(error) => error,
    };

    assert!(error.to_string().contains("must not be a symlink"));
}

#[cfg(unix)]
#[test]
fn private_open_repairs_existing_database_and_sidecars() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("repair.db");
    let connection = Connection::open(&path).expect("open fixture");
    connection
        .pragma_update(None, "journal_mode", "WAL")
        .expect("enable WAL");
    connection
        .execute_batch("CREATE TABLE fixture(value TEXT); INSERT INTO fixture VALUES ('secret');")
        .expect("write fixture");

    for suffix in ["", "-wal", "-shm"] {
        let file = std::path::PathBuf::from(format!("{}{suffix}", path.display()));
        assert!(file.exists(), "fixture sidecar exists: {}", file.display());
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o666))
            .expect("make fixture permissive");
    }

    let opened = super::super::sqlite::open_private(&path).expect("open private");
    assert!(!opened.read_only);
    for suffix in ["", "-wal", "-shm"] {
        let file = std::path::PathBuf::from(format!("{}{suffix}", path.display()));
        let mode = std::fs::metadata(&file)
            .expect("file metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "private mode for {}", file.display());
    }
}

#[cfg(unix)]
#[test]
fn private_open_repairs_permissive_read_only_database_and_sidecars() {
    use std::os::unix::fs::PermissionsExt;

    for database_mode in [0o444, 0o440] {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("immutable.db");
        let connection = Connection::open(&path).expect("open fixture");
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .expect("enable WAL");
        connection
            .execute_batch(
                "CREATE TABLE fixture(value TEXT);\
                 INSERT INTO fixture VALUES ('kept');\
                 PRAGMA wal_checkpoint(FULL);",
            )
            .expect("write and checkpoint fixture");

        for suffix in ["", "-wal", "-shm"] {
            let file = std::path::PathBuf::from(format!("{}{suffix}", path.display()));
            assert!(file.exists(), "fixture file exists: {}", file.display());
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(database_mode))
                .expect("make fixture permissive and read-only");
        }

        let opened = super::super::sqlite::open_private(&path).expect("open immutable");
        assert!(opened.read_only);
        let value = opened
            .connection
            .query_row("SELECT value FROM fixture", [], |row| {
                row.get::<_, String>(0)
            })
            .expect("read immutable fixture");
        assert_eq!(value, "kept");

        for suffix in ["", "-wal", "-shm"] {
            let file = std::path::PathBuf::from(format!("{}{suffix}", path.display()));
            let mode = std::fs::metadata(&file)
                .expect("fixture metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o400, "owner-read-only mode for {}", file.display());
        }
    }
}

#[cfg(unix)]
#[test]
fn private_read_only_filesystem_path_has_no_side_effects() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("immutable.db");
    let connection = Connection::open(&path).expect("open fixture");
    connection
        .execute_batch("CREATE TABLE fixture(value TEXT); INSERT INTO fixture VALUES ('kept');")
        .expect("write fixture");
    drop(connection);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444))
        .expect("make fixture read-only");

    // A unit-test tempdir cannot become a genuine read-only mount without
    // privileges. Exercise the branch selected after statvfs reports ST_RDONLY.
    let opened = super::super::sqlite::open_private_read_only(&path, true)
        .expect("open from read-only filesystem");
    assert!(opened.read_only);
    let value = opened
        .connection
        .query_row("SELECT value FROM fixture", [], |row| {
            row.get::<_, String>(0)
        })
        .expect("read immutable fixture");
    assert_eq!(value, "kept");
    assert_eq!(
        std::fs::metadata(&path)
            .expect("database metadata")
            .permissions()
            .mode()
            & 0o777,
        0o444,
        "read-only filesystem mode must remain unchanged"
    );
    assert!(!std::path::PathBuf::from(format!("{}-wal", path.display())).exists());
    assert!(!std::path::PathBuf::from(format!("{}-shm", path.display())).exists());
}

/// Read-only observation fixtures, all built around one shape: a database
/// whose newest commit is still only in the WAL.
#[cfg(unix)]
mod read_only_observation {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    use rusqlite::Connection;

    use crate::OrbitError;
    use crate::storage::sqlite::{
        ReadOnlyAccess, open_private, open_private_read_only, read_only_access,
    };

    const SQLITE_FILE_SUFFIXES: [&str; 3] = ["", "-wal", "-shm"];

    fn sqlite_file(path: &Path, suffix: &str) -> PathBuf {
        PathBuf::from(format!("{}{suffix}", path.display()))
    }

    /// Write a database where `main_only` is checkpointed back into the main
    /// file and `fresh` is not, then publish the requested files read-only.
    ///
    /// Autocheckpoint is off so the split stays put, and the observed database
    /// is a copy rather than the writer's own files: SQLite shares one
    /// wal-index mapping per file across a process, so a live local writer
    /// would let an observing connection write through it — exactly what a
    /// separate read-only mount cannot do.
    fn publish_uncheckpointed_wal_database(source: &Path, published: &Path, suffixes: &[&str]) {
        let writer = Connection::open(source).expect("open fixture writer");
        writer
            .pragma_update(None, "journal_mode", "WAL")
            .expect("enable WAL");
        writer
            .pragma_update(None, "wal_autocheckpoint", 0)
            .expect("keep new frames in the WAL");
        writer
            .execute_batch(
                "CREATE TABLE main_only(value TEXT);
                 INSERT INTO main_only VALUES ('checkpointed');
                 PRAGMA wal_checkpoint(TRUNCATE);
                 CREATE TABLE fresh(value TEXT);
                 INSERT INTO fresh VALUES ('visible-only-in-wal');",
            )
            .expect("commit fixture state into the WAL");

        publish_read_only(source, published, suffixes);
    }

    fn publish_read_only(source: &Path, published: &Path, suffixes: &[&str]) {
        for suffix in suffixes {
            let from = sqlite_file(source, suffix);
            let to = sqlite_file(published, suffix);
            fs::copy(&from, &to)
                .unwrap_or_else(|error| panic!("publish {}: {error}", from.display()));
            fs::set_permissions(&to, fs::Permissions::from_mode(0o400))
                .unwrap_or_else(|error| panic!("make {} read-only: {error}", to.display()));
        }
    }

    fn published_bytes(path: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        SQLITE_FILE_SUFFIXES
            .iter()
            .map(|suffix| sqlite_file(path, suffix))
            .filter(|file| file.exists())
            .map(|file| {
                let bytes = fs::read(&file)
                    .unwrap_or_else(|error| panic!("read {}: {error}", file.display()));
                (file, bytes)
            })
            .collect()
    }

    fn fixture(name: &str) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let source = dir.path().join(format!("{name}-source.db"));
        let published = dir.path().join(format!("{name}.db"));
        (dir, source, published)
    }

    /// ORB-12090: `immutable=1` ignores the WAL, so a read-only open used to
    /// report the older main-file state as if it were current.
    #[test]
    fn reads_committed_state_held_only_in_the_wal() {
        let (_dir, source, published) = fixture("wal");
        publish_uncheckpointed_wal_database(&source, &published, &SQLITE_FILE_SUFFIXES);
        let before = published_bytes(&published);

        let opened = open_private(&published).expect("open published database");

        assert!(opened.read_only);
        let value: String = opened
            .connection
            .query_row("SELECT value FROM fresh", [], |row| row.get(0))
            .expect("read state only the WAL holds");
        assert_eq!(value, "visible-only-in-wal");

        let denied = opened
            .connection
            .execute("INSERT INTO fresh VALUES ('denied')", [])
            .expect_err("observation must refuse writes");
        assert!(
            OrbitError::Store(denied.to_string()).is_readonly_or_access_failure(),
            "{denied}"
        );

        drop(opened);
        assert_eq!(
            published_bytes(&published),
            before,
            "observing must not change the database, WAL, or SHM"
        );
    }

    /// The control for the branch above: once every frame is checkpointed back
    /// into the main file, nothing can hide behind the WAL and the
    /// sidecar-free mode stays in use.
    #[test]
    fn checkpointed_database_stays_immutable() {
        let (_dir, source, published) = fixture("checkpointed");
        let writer = Connection::open(&source).expect("open fixture writer");
        writer
            .pragma_update(None, "journal_mode", "WAL")
            .expect("enable WAL");
        writer
            .execute_batch(
                "CREATE TABLE fresh(value TEXT);
                 INSERT INTO fresh VALUES ('visible-only-in-wal');
                 PRAGMA wal_checkpoint(TRUNCATE);",
            )
            .expect("commit and checkpoint fixture state");
        publish_read_only(&source, &published, &SQLITE_FILE_SUFFIXES);
        let before = published_bytes(&published);

        assert_eq!(
            read_only_access(&published).expect("classify a checkpointed database"),
            ReadOnlyAccess::Immutable,
            "an empty WAL cannot hide committed pages"
        );
        let opened = open_private(&published).expect("open published database");
        assert!(opened.read_only);
        let value: String = opened
            .connection
            .query_row("SELECT value FROM fresh", [], |row| row.get(0))
            .expect("read checkpointed state");
        assert_eq!(value, "visible-only-in-wal");

        drop(opened);
        assert_eq!(published_bytes(&published), before);
    }

    /// A WAL with frames but no published wal-index cannot be read without
    /// creating one. Fail closed: the main file alone would be a silent, stale
    /// success.
    #[test]
    fn wal_without_read_support_fails_closed() {
        let (_dir, source, published) = fixture("no-shm");
        publish_uncheckpointed_wal_database(&source, &published, &["", "-wal"]);
        let before = published_bytes(&published);

        let error = match open_private(&published) {
            Ok(_) => panic!("a WAL without read support must not read as current"),
            Err(error) => error,
        };

        let message = error.to_string();
        assert!(message.contains("'-shm' wal-index is missing"), "{message}");
        assert!(message.contains("wal_checkpoint(TRUNCATE)"), "{message}");
        assert!(
            !error.is_readonly_or_access_failure(),
            "a stale-read refusal must not be tolerated as a write denial: {message}"
        );
        assert!(
            !sqlite_file(&published, "-shm").exists(),
            "observation must not publish a wal-index"
        );
        assert_eq!(published_bytes(&published), before);
    }

    #[test]
    fn symlinked_sidecar_is_rejected() {
        let (dir, source, published) = fixture("linked");
        publish_uncheckpointed_wal_database(&source, &published, &SQLITE_FILE_SUFFIXES);
        let wal = sqlite_file(&published, "-wal");
        let elsewhere = dir.path().join("elsewhere-wal");
        fs::rename(&wal, &elsewhere).expect("move the WAL out of the published file set");
        std::os::unix::fs::symlink(&elsewhere, &wal).expect("link the WAL back");

        let error = match open_private_read_only(&published, true) {
            Ok(_) => panic!("a symlinked sidecar must not be trusted"),
            Err(error) => error,
        };

        assert!(
            error.to_string().contains("sidecar must be a regular file"),
            "{error}"
        );
    }
}
