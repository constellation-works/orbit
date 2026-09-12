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

#[cfg(unix)]
#[test]
fn wal_file_set_lease_keeps_sidecar_identity_stable() {
    use std::os::unix::fs::MetadataExt;

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("leased.db");
    let connection = Connection::open(&path).expect("create database");
    connection
        .pragma_update(None, "journal_mode", "WAL")
        .expect("enable WAL");
    connection
        .execute_batch("CREATE TABLE fixture(value INTEGER); INSERT INTO fixture VALUES (1);")
        .expect("seed database");
    drop(connection);
    assert!(
        ["-wal", "-shm"]
            .iter()
            .all(
                |suffix| !std::path::PathBuf::from(format!("{}{suffix}", path.display())).exists()
            ),
        "the pre-repair lifecycle gap requires SQLite last-close cleanup"
    );

    let lease = super::super::sqlite::lease_wal_file_set(&path)
        .expect("open WAL lease")
        .expect("WAL database needs a lease");
    let identities = ["-wal", "-shm"].map(|suffix| {
        std::fs::metadata(format!("{}{suffix}", path.display()))
            .expect("lease materialized sidecar")
            .ino()
    });

    let writer = Connection::open(&path).expect("open concurrent writer");
    writer
        .execute("INSERT INTO fixture VALUES (2)", [])
        .expect("write through leased file set");
    drop(writer);

    assert_eq!(
        ["-wal", "-shm"].map(|suffix| {
            std::fs::metadata(format!("{}{suffix}", path.display()))
                .expect("leased sidecar remains linked")
                .ino()
        }),
        identities,
        "last-close cleanup must not replace descriptor-backed sidecars"
    );
    drop(lease);
}

#[test]
fn wal_file_set_lease_keeps_missing_and_corrupt_failures_attributable() {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().expect("tempdir");
    let missing = dir.path().join("missing.db");
    assert!(
        super::super::sqlite::lease_wal_file_set(&missing)
            .expect("missing database is not a lease error")
            .is_none()
    );

    let corrupt = dir.path().join("corrupt.db");
    std::fs::write(&corrupt, b"not a SQLite database").expect("write corrupt database");
    let error = super::super::sqlite::lease_wal_file_set(&corrupt)
        .expect_err("corrupt database must fail closed");
    let message = error.to_string();
    assert!(
        message.contains(&corrupt.display().to_string()),
        "{message}"
    );
    assert!(message.contains("code=NotADatabase"), "{message}");
    assert!(message.contains("extended_code=26"), "{message}");

    #[cfg(unix)]
    {
        let unreadable = dir.path().join("unreadable.db");
        Connection::open(&unreadable).expect("create unreadable fixture");
        std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000))
            .expect("remove database access");
        let error = super::super::sqlite::lease_wal_file_set(&unreadable)
            .expect_err("unreadable database must fail closed");
        std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o600))
            .expect("restore database access");
        let message = error.to_string();
        assert!(
            message.contains(&unreadable.display().to_string()),
            "{message}"
        );
        assert!(message.contains("code=CannotOpen"), "{message}");
        assert!(message.contains("extended_code=14"), "{message}");
    }
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
/// another process may commit to while this process only observes it.
#[cfg(unix)]
mod read_only_observation {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    use rusqlite::Connection;

    use crate::OrbitError;
    use crate::storage::sqlite::{
        ObservationCurrency, ObservationRequirement, observation_currency, open_private,
        open_private_read_only,
    };

    const SQLITE_FILE_SUFFIXES: [&str; 3] = ["", "-wal", "-shm"];

    /// How long a fixture waits for its writer process to reach the next step.
    const WRITER_TIMEOUT: Duration = Duration::from_secs(30);

    /// Database the child writer opens, and the directory both halves use to
    /// hand steps back and forth.
    const WRITER_DATABASE_ENV: &str = "ORBIT_TEST_OBSERVATION_DATABASE";
    const WRITER_CONTROL_ENV: &str = "ORBIT_TEST_OBSERVATION_CONTROL";

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

    /// Make an existing file set read-only for this process, leaving the files
    /// where they are so a writer that already opened them keeps its access.
    fn publish_in_place_read_only(path: &Path) {
        for suffix in SQLITE_FILE_SUFFIXES {
            let file = sqlite_file(path, suffix);
            if file.exists() {
                fs::set_permissions(&file, fs::Permissions::from_mode(0o400))
                    .unwrap_or_else(|error| panic!("make {} read-only: {error}", file.display()));
            }
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

    /// Write a WAL database holding one checkpointed row, so the main file is
    /// complete and the `-wal` carries no frames. The writer closes, which
    /// removes both sidecars.
    fn write_checkpointed_database(path: &Path) {
        let writer = Connection::open(path).expect("open fixture writer");
        writer
            .pragma_update(None, "journal_mode", "WAL")
            .expect("enable WAL");
        writer
            .pragma_update(None, "wal_autocheckpoint", 0)
            .expect("keep later frames in the WAL");
        writer
            .execute_batch(
                "CREATE TABLE observed(value TEXT);
                 INSERT INTO observed VALUES ('checkpointed');
                 PRAGMA wal_checkpoint(TRUNCATE);",
            )
            .expect("commit and checkpoint the fixture row");
    }

    fn rows(connection: &Connection) -> Vec<String> {
        let mut statement = connection
            .prepare("SELECT value FROM observed ORDER BY rowid")
            .expect("prepare observation query");
        statement
            .query_map([], |row| row.get::<_, String>(0))
            .expect("run observation query")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect observed rows")
    }

    fn wait_for(marker: &Path, step: &str) {
        let deadline = Instant::now() + WRITER_TIMEOUT;
        while Instant::now() < deadline {
            if marker.exists() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("the writer process never reached '{step}'");
    }

    /// A writer holding the database open in another process.
    ///
    /// It has to be another process: SQLite shares one wal-index mapping per
    /// file within a process, so an in-process writer would let an observing
    /// connection ride on the writer's writable mapping — exactly what a
    /// separate read-only mount cannot do.
    ///
    /// The child opens the database read-write and then waits, so the fixture
    /// can make the files read-only for this process while the child keeps
    /// committing through the descriptors it already holds. That is the shape
    /// of the reported failure: a reader behind a read-only view of a database
    /// a writer still reaches from writable storage.
    struct ExternalWriter {
        child: Child,
        control: PathBuf,
        commits: usize,
    }

    impl ExternalWriter {
        /// Re-runs this test binary as the writer. The filter below names
        /// [`external_writer_process`] by its module path; renaming that test
        /// without updating it makes the child run nothing, and every fixture
        /// then fails waiting for 'ready'.
        fn start(database: &Path, control: PathBuf) -> Self {
            fs::create_dir_all(&control).expect("create writer control directory");
            let child = Command::new(std::env::current_exe().expect("test binary path"))
                .args([
                    "--exact",
                    "storage::tests::sqlite::read_only_observation::external_writer_process",
                    "--ignored",
                ])
                .env(WRITER_DATABASE_ENV, database)
                .env(WRITER_CONTROL_ENV, &control)
                .stdout(Stdio::null())
                .spawn()
                .expect("spawn the writer process");

            let writer = Self {
                child,
                control,
                commits: 0,
            };
            wait_for(&writer.control.join("ready"), "ready");
            writer
        }

        /// Commit one row and wait for the writer to confirm it.
        fn commit(&mut self, value: &str) {
            self.commits += 1;
            let request = self.control.join(format!("commit-{}", self.commits));
            let confirmation = self.control.join(format!("committed-{}", self.commits));
            fs::write(&request, value).expect("request a commit");
            wait_for(&confirmation, "commit");
            let outcome = fs::read_to_string(&confirmation).expect("read commit outcome");
            assert_eq!(outcome, "ok", "the writer process failed to commit");
        }
    }

    impl Drop for ExternalWriter {
        fn drop(&mut self) {
            let _ = fs::write(self.control.join("stop"), b"");
            let _ = self.child.wait();
        }
    }

    /// The child half of [`ExternalWriter`]; never part of an ordinary run.
    #[test]
    #[ignore = "spawned by ExternalWriter as a separate writer process"]
    fn external_writer_process() {
        let Ok(database) = std::env::var(WRITER_DATABASE_ENV) else {
            return;
        };
        let control = PathBuf::from(std::env::var(WRITER_CONTROL_ENV).expect("control directory"));

        let connection = Connection::open(&database).expect("open the database for writing");
        connection
            .pragma_update(None, "wal_autocheckpoint", 0)
            .expect("keep committed frames in the WAL");
        // Force the wal-index into existence before the fixture publishes the
        // file set, so the observing side sees a real `-shm`.
        rows(&connection);
        fs::write(control.join("ready"), b"").expect("announce readiness");

        let stop = control.join("stop");
        let deadline = Instant::now() + WRITER_TIMEOUT;
        let mut commits = 0;
        while Instant::now() < deadline && !stop.exists() {
            let request = control.join(format!("commit-{}", commits + 1));
            if !request.exists() {
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
            commits += 1;
            let value = fs::read_to_string(&request).expect("read the requested value");
            let outcome = match connection.execute("INSERT INTO observed VALUES (?1)", [&value]) {
                Ok(_) => "ok".to_string(),
                Err(error) => format!("failed: {error}"),
            };
            fs::write(control.join(format!("committed-{commits}")), outcome)
                .expect("confirm the commit");
        }
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

    /// ORB-12092: a checkpointed database with a published wal-index reads as
    /// current, so a commit another process makes after the open is visible to
    /// the next query. `immutable=1` used to be chosen here — the empty WAL
    /// looked like proof that nothing could hide — and a read-only mount then
    /// reported the pre-open state forever.
    ///
    /// The observing side never writes: it opens files this process cannot
    /// write, and every byte of the file set is compared around each of its
    /// reads.
    #[test]
    fn checkpointed_wal_observes_a_commit_made_after_the_open() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("late-commit.db");
        write_checkpointed_database(&path);

        let mut writer = ExternalWriter::start(&path, dir.path().join("control"));
        assert_eq!(
            fs::metadata(sqlite_file(&path, "-wal"))
                .expect("published WAL")
                .len(),
            0,
            "the fixture WAL must be empty at open"
        );
        // Read-only for this process from here on; the writer keeps committing
        // through the descriptors it opened above.
        publish_in_place_read_only(&path);
        let before_observation = published_bytes(&path);

        let opened = open_private(&path).expect("observe the published database");

        assert!(opened.read_only);
        assert_eq!(
            opened.currency,
            ObservationCurrency::Live,
            "a published wal-index must be read live, not frozen at open"
        );
        assert_eq!(rows(&opened.connection), ["checkpointed"]);
        assert_eq!(
            published_bytes(&path),
            before_observation,
            "opening and reading must not change the database, WAL, or SHM"
        );

        writer.commit("committed-after-the-open");
        let after_commit = published_bytes(&path);

        assert_eq!(
            rows(&opened.connection),
            ["checkpointed", "committed-after-the-open"],
            "the observation must show the commit that landed after it was opened"
        );

        let denied = opened
            .connection
            .execute("INSERT INTO observed VALUES ('denied')", [])
            .expect_err("observation must refuse writes");
        assert!(
            OrbitError::Store(denied.to_string()).is_readonly_or_access_failure(),
            "{denied}"
        );

        drop(opened);
        assert_eq!(
            published_bytes(&path),
            after_commit,
            "only the writer may change the file set"
        );
    }

    /// A database published without its sidecars can only be read main-file
    /// first: a reader that may not create a wal-index cannot see a WAL. The
    /// open says so, and a caller that requires current state is refused
    /// outright instead of being handed the degraded read.
    #[test]
    fn wal_created_after_the_open_is_invisible_to_a_main_file_read() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("unindexed.db");
        write_checkpointed_database(&path);
        for suffix in ["-wal", "-shm"] {
            assert!(
                !sqlite_file(&path, suffix).exists(),
                "a closed writer leaves no {suffix} sidecar"
            );
        }
        let before_observation = published_bytes(&path);

        let refused = match observation_currency(&path, ObservationRequirement::CurrentState) {
            Ok(currency) => panic!("current state is not readable here, but got {currency:?}"),
            Err(error) => error.to_string(),
        };
        assert!(
            refused.contains("no published '-shm' wal-index"),
            "{refused}"
        );

        // The files stay writable so a writer can still attach; this models the
        // read-only-mount branch, where the mount is what denies the writes.
        let opened =
            open_private_read_only(&path, true).expect("observe a database without sidecars");

        assert!(opened.read_only);
        assert_eq!(
            opened.currency,
            ObservationCurrency::MainFileOnly,
            "a store that accepts a published file set must still be told what it got"
        );
        assert_eq!(rows(&opened.connection), ["checkpointed"]);
        assert_eq!(
            published_bytes(&path),
            before_observation,
            "a main-file read must not create a WAL or wal-index"
        );

        let mut writer = ExternalWriter::start(&path, dir.path().join("control"));
        writer.commit("committed-after-the-main-file-read");
        assert!(
            sqlite_file(&path, "-wal").exists(),
            "the writer must have created a WAL after the observation opened"
        );

        assert_eq!(
            rows(&opened.connection),
            ["checkpointed"],
            "a main-file read cannot see a WAL that appears after it opened"
        );

        let reopened = open_private_read_only(&path, true).expect("re-observe the database");
        assert_eq!(
            reopened.currency,
            ObservationCurrency::Live,
            "the wal-index the writer published makes live reads possible again"
        );
        assert_eq!(
            rows(&reopened.connection),
            ["checkpointed", "committed-after-the-main-file-read"],
            "reopening is the documented way to leave a stale main-file read behind"
        );
    }

    /// A rollback-journal database needs no wal-index at all, so it is read
    /// live and creates nothing — the compatibility the sidecar-free mode used
    /// to provide, without the frozen view it came with.
    #[test]
    fn rollback_journal_database_observes_a_commit_made_after_the_open() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("rollback.db");
        let writer = Connection::open(&path).expect("open fixture writer");
        writer
            .execute_batch(
                "CREATE TABLE observed(value TEXT); INSERT INTO observed VALUES ('journalled');",
            )
            .expect("commit the fixture row");
        drop(writer);

        assert_eq!(
            observation_currency(&path, ObservationRequirement::CurrentState)
                .expect("classify a rollback-journal database"),
            ObservationCurrency::Live,
        );
        let opened = open_private_read_only(&path, true).expect("observe the database");
        assert_eq!(rows(&opened.connection), ["journalled"]);

        let mut external = ExternalWriter::start(&path, dir.path().join("control"));
        external.commit("committed-after-the-open");

        assert_eq!(
            rows(&opened.connection),
            ["journalled", "committed-after-the-open"],
        );
        for suffix in ["-wal", "-shm"] {
            assert!(
                !sqlite_file(&path, suffix).exists(),
                "observing a rollback-journal database must not create a {suffix} sidecar"
            );
        }
    }

    /// A wal-index published without the WAL it indexes describes frames this
    /// process cannot see. Fail closed rather than reporting the main file as
    /// current — and without letting SQLite create the missing WAL.
    #[test]
    fn published_wal_index_without_its_wal_fails_closed() {
        let (_dir, source, published) = fixture("orphan-index");
        publish_uncheckpointed_wal_database(&source, &published, &["", "-shm"]);
        let before = published_bytes(&published);

        let error = match open_private(&published) {
            Ok(_) => panic!("an orphaned wal-index must not read as current"),
            Err(error) => error,
        };

        let message = error.to_string();
        assert!(
            message.contains("published without the '-wal' sidecar it indexes"),
            "{message}"
        );
        assert!(
            !sqlite_file(&published, "-wal").exists(),
            "observation must not create a WAL"
        );
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
