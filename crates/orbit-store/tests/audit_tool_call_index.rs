//! Additive migration of an existing store, using the public open boundary.
#![allow(clippy::expect_used, clippy::unwrap_used, missing_docs)]
use orbit_common::{process, test_env};
use orbit_store::Store;
use rusqlite::Connection;
use std::process::Command;
use std::time::Duration;

#[test]
fn existing_store_gains_tool_call_index_without_rewriting_audit_rows() {
    const TEST: &str = "existing_store_gains_tool_call_index_without_rewriting_audit_rows";
    const MARKER: &str = "ORBIT_AUDIT_INDEX_TEST_CHILD";
    if std::env::var(MARKER).as_deref() != Ok(TEST) {
        let root = tempfile::tempdir_in(test_env::canonical_temp_dir()).unwrap();
        let mut command = Command::new(std::env::current_exe().unwrap());
        test_env::clear_inherited_authority(|key| {
            command.env_remove(key);
        });
        command
            .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
            .env(MARKER, TEST)
            .env("HOME", root.path())
            .env("USERPROFILE", root.path())
            .current_dir(root.path());
        let result =
            process::run_bounded_capped(&mut command, Duration::from_secs(60), 256 * 1024).unwrap();
        test_env::assert_child_test_passed(TEST, result.status, result.stdout, result.stderr);
        return;
    }
    let root = tempfile::tempdir_in(test_env::canonical_temp_dir()).unwrap();
    let path = root.path().join("store.sqlite");
    drop(Store::open(&path).unwrap());
    let conn = Connection::open(&path).unwrap();
    // Recreate schema v36 so the next open applies the index migration at v37
    // and the job-state migration at v38. Compatibility is rewritten by the
    // opener.
    conn.execute_batch(
        "DROP INDEX idx_audit_events_command_subcommand_timestamp;
        DROP TABLE job_run_states;
        ALTER TABLE job_runs ADD COLUMN pipeline_state_json TEXT;
        DELETE FROM schema_meta WHERE key IN ('migration.v0037', 'migration.v0038');
        INSERT INTO audit_events (execution_id, timestamp, command, role, status,
            exit_code, duration_ms, working_directory, pid)
        VALUES ('preserved', '2026-10-01T00:00:00Z', 'tool', 'codex', 'failure', 1, 1, '.', 1);",
    )
    .unwrap();
    drop(conn);
    for _ in 0..2 {
        drop(Store::open(&path).unwrap());
        let conn = Connection::open(&path).unwrap();
        let columns: Vec<String> = conn
            .prepare("PRAGMA index_info(idx_audit_events_command_subcommand_timestamp)")
            .unwrap()
            .query_map([], |row| row.get(2))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(columns, ["command", "subcommand", "timestamp"]);
        let row: (String, String) = conn
            .query_row("SELECT execution_id, status FROM audit_events", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert_eq!(row, ("preserved".into(), "failure".into()));
        let index_migration: String = conn
            .query_row(
                "SELECT value FROM schema_meta WHERE key = 'migration.v0037'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(index_migration, "audit_tool_call_index");
        let state_migration: String = conn
            .query_row(
                "SELECT value FROM schema_meta WHERE key = 'migration.v0038'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(state_migration, "job_run_states");
    }
}
