//! Unit tests for `index` — sibling layout under vector/tests/.
//!
//! The contract under test is what an Orbit host may do with an optional index
//! it could not open: continue when the storage refused it, keep failing when
//! the index exists and is broken, and never report either as an empty corpus.

use crate::vector::index::SemanticIndex;

const UNAVAILABLE_MARKER: &str = "is unavailable";

/// An ordinary writable state directory still yields a functional index.
#[test]
fn writable_state_opens_a_usable_index() {
    let root = tempfile::tempdir().expect("tempdir");
    let path = root.path().join("state/semantic.db");

    let index = SemanticIndex::open(&path).expect("open writable index");

    let store = index.store().expect("writable index must be ready");
    assert!(
        store.model_ids().expect("query a ready index").is_empty(),
        "a freshly created index carries no embedding rows yet"
    );
    assert!(
        path.exists(),
        "opening writable state must create the index"
    );
}

/// A database that exists but cannot be read is real state that failed, not an
/// index that was never built. It must keep propagating.
#[test]
fn existing_malformed_database_still_fails_closed() {
    let root = tempfile::tempdir().expect("tempdir");
    let path = root.path().join("semantic.db");
    std::fs::write(&path, b"this is not a SQLite database").expect("write malformed index");

    let Err(error) = SemanticIndex::open(&path) else {
        panic!("a malformed index must not be tolerated");
    };

    assert!(
        !error.to_string().contains(UNAVAILABLE_MARKER),
        "a broken index must not be reported as an absent one: {error}"
    );
    assert!(
        error.to_string().contains("not a database"),
        "the SQLite diagnostic must survive: {error}"
    );
}

/// The startup path behind the read-only mount: the optional index is absent
/// and the state directory refuses to create it. The host must still open, and
/// every semantic caller must be told why there is nothing to consult.
///
/// See [`read_only_store_without_a_vector_schema_is_unavailable_not_ready`] for
/// the same refusal reached one step later, on a database that does exist.
#[cfg(unix)]
#[test]
fn absent_index_on_unwritable_state_reports_unavailable_instead_of_failing() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().expect("tempdir");
    let state = root.path().join("state");
    std::fs::create_dir_all(&state).expect("create state directory");
    let path = state.join("semantic.db");
    let _guard = ReadOnlyDir::hold(&state);
    if std::fs::File::create(state.join("probe")).is_ok() {
        // Running as a user that ignores the mode bits (typically root) makes
        // this fixture unable to reproduce the denial it is about.
        return;
    }

    let index = SemanticIndex::open(&path).expect("an absent optional index must not fail startup");

    let Err(error) = index.store() else {
        panic!("an unavailable index must not answer as an empty corpus");
    };
    let message = error.to_string();
    assert!(
        message.contains(UNAVAILABLE_MARKER) && message.contains("semantic.db"),
        "the unavailability must name the index: {message}"
    );
    assert!(
        message.contains("orbit semantic index"),
        "the unavailability must carry its remediation: {message}"
    );
    assert!(
        !path.exists(),
        "reporting unavailability must not create the index"
    );
    assert_eq!(
        std::fs::metadata(&state)
            .expect("state metadata")
            .permissions()
            .mode()
            & 0o777,
        0o500,
        "opening must leave the protected state directory unchanged"
    );
}

/// An index that exists behind a directory this caller cannot enter is also a
/// refusal, and must be described as one. Classifying it by asking whether the
/// path exists would be wrong twice over: the inspection itself is denied, and
/// the answer would call real state absent.
#[cfg(unix)]
#[test]
fn inaccessible_existing_index_is_reported_as_refused_not_absent() {
    let root = tempfile::tempdir().expect("tempdir");
    let state = root.path().join("state");
    std::fs::create_dir_all(&state).expect("create state directory");
    let path = state.join("semantic.db");
    SemanticIndex::open(&path).expect("build the index while the directory is reachable");
    let _guard = ReadOnlyDir::hold_with_mode(&state, 0o000);
    if std::fs::metadata(&path).is_ok() {
        // A user that ignores the mode bits (typically root) can still enter
        // the directory, so the denial this fixture is about cannot happen.
        return;
    }

    let index = SemanticIndex::open(&path).expect("a denied optional index must not fail startup");

    let Err(error) = index.store() else {
        panic!("an index this process cannot reach must not be handed out as ready");
    };
    let message = error.to_string();
    assert!(
        message.contains(UNAVAILABLE_MARKER),
        "the denial must be reported as unavailability: {message}"
    );
    assert!(
        !message.contains("does not exist"),
        "a denied inspection must not claim the index is absent: {message}"
    );
}

/// A read-only database carrying no vector index at all is not a usable store.
/// Opening its connection succeeded, but every query would fail on a missing
/// table and no writer could land a row, so it must be reported unavailable
/// rather than handed out as ready — a `Ready` store is what a long-lived host
/// starts its embed worker against.
#[cfg(unix)]
#[test]
fn read_only_store_without_a_vector_schema_is_unavailable_not_ready() {
    let root = tempfile::tempdir().expect("tempdir");
    let path = root.path().join("semantic.db");
    rusqlite::Connection::open(&path)
        .expect("create an empty SQLite database")
        .execute_batch("CREATE TABLE unrelated(id INTEGER PRIMARY KEY)")
        .expect("seed a database that carries no vector index");
    if !deny_writes(&path) {
        return;
    }

    let index =
        SemanticIndex::open(&path).expect("an unusable optional index must not fail startup");

    let Err(error) = index.store() else {
        panic!("a database with no vector index must not be handed out as ready");
    };
    assert!(
        error.to_string().contains(UNAVAILABLE_MARKER),
        "the refusal must be reported as unavailability: {error}"
    );
}

/// The other side of that rule, and the [ORB-12090] read-only mount case: an
/// index that already exists stays readable when its schema call cannot write.
/// Here the pre-[ORB-11695] inline layout cannot be migrated in place, and the
/// store must still open rather than degrade a readable index to unavailable.
#[cfg(unix)]
#[test]
fn read_only_store_with_an_unmigratable_index_stays_ready() {
    let root = tempfile::tempdir().expect("tempdir");
    let path = root.path().join("semantic.db");
    rusqlite::Connection::open(&path)
        .expect("create the legacy index")
        .execute_batch(
            r#"
                CREATE VIRTUAL TABLE corpus_fts USING fts5(
                    source_kind UNINDEXED,
                    source_id UNINDEXED,
                    field UNINDEXED,
                    content,
                    tokenize = 'porter unicode61 remove_diacritics 2'
                );
                INSERT INTO corpus_fts(source_kind, source_id, field, content)
                VALUES ('task', 'T1', 'purpose', 'alpha neutrino');
            "#,
        )
        .expect("seed the pre-migration inline layout");
    if !deny_writes(&path) {
        return;
    }

    let index = SemanticIndex::open(&path).expect("open the read-only legacy index");

    let store = index
        .store()
        .expect("an existing index must stay readable when its migration cannot run");
    let connection = store.connection();
    let conn = connection.lock().expect("lock the vector connection");
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM corpus_fts WHERE corpus_fts MATCH 'neutrino'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .expect("read the legacy corpus"),
        1,
        "the unmigrated rows must still be readable"
    );
}

/// Make `path` refuse writes through its permission bits, reporting whether the
/// running user is actually bound by them.
#[cfg(unix)]
fn deny_writes(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o400))
        .expect("make the database read-only");
    // A user that ignores the mode bits (typically root) cannot reproduce the
    // write denial these fixtures are about.
    std::fs::OpenOptions::new().write(true).open(path).is_err()
}

/// Restricts a directory's mode and restores it, so the temporary directory can
/// be removed even when an assertion unwinds out of the test.
#[cfg(unix)]
struct ReadOnlyDir {
    path: std::path::PathBuf,
    original: std::fs::Permissions,
}

#[cfg(unix)]
impl ReadOnlyDir {
    fn hold(path: &std::path::Path) -> Self {
        Self::hold_with_mode(path, 0o500)
    }

    fn hold_with_mode(path: &std::path::Path, mode: u32) -> Self {
        use std::os::unix::fs::PermissionsExt;

        let original = std::fs::metadata(path)
            .expect("read directory permissions")
            .permissions();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
            .expect("restrict the state directory");
        Self {
            path: path.to_path_buf(),
            original,
        }
    }
}

#[cfg(unix)]
impl Drop for ReadOnlyDir {
    fn drop(&mut self) {
        let _ = std::fs::set_permissions(&self.path, self.original.clone());
    }
}
