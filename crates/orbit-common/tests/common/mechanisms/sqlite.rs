//! Private SQLite opens refuse redirected paths, and a read-only observation
//! sees commits another process makes after it opened.

use orbit_common::storage::sqlite::open_private;
#[cfg(unix)]
use rusqlite::Connection;

#[test]
fn private_open_rejects_parent_directory_traversal() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("nested").join("..").join("escape.db");

    let error = match open_private(&path) {
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

    let error = match open_private(&link) {
        Ok(_) => panic!("reject symlink database"),
        Err(error) => error,
    };

    assert!(error.to_string().contains("must not be a symlink"));
}

#[cfg(unix)]
#[test]
fn private_open_rejects_symlinked_sidecars_without_touching_their_targets() {
    use std::os::unix::fs::PermissionsExt;

    for suffix in ["-wal", "-shm"] {
        for database_exists in [true, false] {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("linked.db");
            if database_exists {
                Connection::open(&path)
                    .and_then(|connection| connection.execute_batch("CREATE TABLE fixture(v);"))
                    .expect("create writable database");
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
                    .expect("make database permissive");
            }
            let target = dir.path().join("unrelated.txt");
            std::fs::write(&target, b"unrelated bytes").expect("write target");
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644))
                .expect("make target permissive");
            let sidecar = std::path::PathBuf::from(format!("{}{suffix}", path.display()));
            std::os::unix::fs::symlink(&target, &sidecar).expect("link sidecar");

            let error = match open_private(&path) {
                Ok(_) => panic!("a {suffix} symlink must fail the writable open"),
                Err(error) => error,
            };

            assert!(
                error.to_string().contains("must not be a symlink"),
                "{suffix}: {error}"
            );
            assert_eq!(
                std::fs::read(&target).expect("read target"),
                b"unrelated bytes",
                "{suffix} target bytes (database existed: {database_exists})"
            );
            assert_eq!(
                std::fs::metadata(&target)
                    .expect("target metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o644,
                "{suffix} target mode (database existed: {database_exists})"
            );
        }
    }
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

    use orbit_common::StorageLayer;
    use orbit_common::storage::sqlite::{ObservationCurrency, open_private, sqlite_error};

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
        /// without updating it fails the entry-point guard before spawning.
        /// The ready sentinel then proves the writer actually started.
        fn start(database: &Path, control: PathBuf) -> Self {
            orbit_common::test_env::assert_child_test_exists(
                "sqlite::read_only_observation::external_writer_process",
            );
            fs::create_dir_all(&control).expect("create writer control directory");
            let child = Command::new(std::env::current_exe().expect("test binary path"))
                .args([
                    "--exact",
                    "sqlite::read_only_observation::external_writer_process",
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
        assert_eq!(
            denied.sqlite_error_code(),
            Some(rusqlite::ErrorCode::ReadOnly),
            "{denied}"
        );
        assert!(
            sqlite_error(StorageLayer::Store, &denied, denied.to_string())
                .is_readonly_or_access_failure(),
            "SQLITE_READONLY must classify as read-only through the translator: {denied}"
        );

        drop(opened);
        assert_eq!(
            published_bytes(&path),
            after_commit,
            "only the writer may change the file set"
        );
    }
}

/// A SQLite busy result keeps its lock-busy class through the translators, and
/// reads, codes and serializes exactly as the `Store` or `Migration` error it
/// would otherwise have been; quoting SQLite's words is not the class.
#[test]
fn busy_sqlite_error_classifies_as_lock_busy_and_reads_as_its_layer() {
    use orbit_common::storage::sqlite::{sqlite_error, sqlite_store_error};
    use orbit_common::{OrbitError, StorageLayer};

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("busy.db");
    let holder = rusqlite::Connection::open(&path).expect("open holder");
    holder
        .execute_batch("CREATE TABLE t(v TEXT); BEGIN IMMEDIATE;")
        .expect("hold the writer lock");
    let busy = || {
        let waiter = rusqlite::Connection::open(&path).expect("open waiter");
        waiter
            .busy_timeout(std::time::Duration::ZERO)
            .expect("no busy wait");
        waiter
            .execute_batch("BEGIN IMMEDIATE;")
            .expect_err("the writer lock is held")
    };

    let error = busy();
    assert_eq!(
        error.sqlite_error_code(),
        Some(rusqlite::ErrorCode::DatabaseBusy),
        "{error}"
    );
    let text = error.to_string();
    let store = sqlite_store_error(error);
    assert!(store.is_lock_busy(), "{store:?}");
    assert!(!store.is_readonly_or_access_failure(), "{store:?}");
    assert_eq!(store.storage_layer(), Some(StorageLayer::Store));
    let plain = OrbitError::Store(text.clone());
    assert_eq!(store.to_string(), plain.to_string());
    assert_eq!(
        serde_json::to_value(&store).expect("serialize"),
        serde_json::to_value(&plain).expect("serialize"),
        "the classified error must serialize as Store(string)"
    );

    let migration = sqlite_error(StorageLayer::Migration, &busy(), format!("apply: {text}"));
    assert!(migration.is_lock_busy(), "{migration:?}");
    let plain = OrbitError::Migration(format!("apply: {text}"));
    assert_eq!(migration.to_string(), plain.to_string());
    assert_eq!(
        serde_json::to_value(&migration).expect("serialize"),
        serde_json::to_value(&plain).expect("serialize"),
        "the classified error must serialize as Migration(string)"
    );

    assert!(
        !OrbitError::Store(text).is_lock_busy(),
        "a Store error that only quotes SQLite's words is not lock-busy"
    );
}
