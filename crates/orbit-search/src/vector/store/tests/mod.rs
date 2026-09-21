//! Unit tests for `store` (vector SQLite index), split by source file per
//! `docs/design-patterns/test_layout.md` (ORB-00230 sibling migration).
//!
//! The parent `store/mod.rs` declares `#[cfg(test)] mod tests;`.

#![allow(missing_docs)]

mod queries;
mod schema;
mod tasks;
mod upsert;

/// [ORB-10004] `VectorStore::open` must apply the shared Orbit connection
/// defaults from `orbit_common::storage::sqlite` — historically this store
/// drifted (it was missing `foreign_keys` and `synchronous`).
mod open_pragmas {
    use crate::vector::store::VectorStore;

    #[test]
    fn open_applies_shared_pragma_defaults() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = VectorStore::open(&dir.path().join("semantic.db")).expect("open store");

        let conn_arc = store.connection();
        let conn = conn_arc.lock().expect("lock connection");
        let journal_mode = conn
            .pragma_query_value(None, "journal_mode", |row| row.get::<_, String>(0))
            .expect("journal_mode");
        assert_eq!(journal_mode.to_lowercase(), "wal");
        let pragma_i64 = |name: &str| -> i64 {
            conn.pragma_query_value(None, name, |row| row.get::<_, i64>(0))
                .expect("query pragma")
        };
        assert_eq!(pragma_i64("busy_timeout"), 5_000);
        assert_eq!(pragma_i64("foreign_keys"), 1);
        // synchronous=NORMAL reports as 1.
        assert_eq!(pragma_i64("synchronous"), 1);
    }

    #[cfg(unix)]
    #[test]
    fn open_creates_private_vector_state_under_permissive_umask() {
        use std::os::unix::fs::PermissionsExt;

        const CHILD_MARKER: &str = "ORBIT_TEST_PRIVATE_VECTOR_SQLITE";
        if std::env::var_os(CHILD_MARKER).is_none() {
            let status = std::process::Command::new("sh")
                .args(["-c", "umask 000; exec \"$@\"", "sh"])
                .arg(std::env::current_exe().expect("current test executable"))
                .arg("open_creates_private_vector_state_under_permissive_umask")
                .env(CHILD_MARKER, "1")
                .status()
                .expect("run test under permissive umask");
            assert!(status.success(), "permissive-umask child failed");
            return;
        }

        let root = tempfile::tempdir().expect("tempdir");
        let state_dir = root.path().join("private/state");
        let path = state_dir.join("semantic.db");
        let store = VectorStore::open(&path).expect("open store");
        let connection = store.connection();
        let conn = connection.lock().expect("lock vector connection");
        conn.execute_batch("BEGIN IMMEDIATE; COMMIT;")
            .expect("touch vector WAL");

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
}
