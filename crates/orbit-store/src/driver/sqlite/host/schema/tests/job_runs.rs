// v38 `job_run_states`: pipeline state leaves `job_runs` for a side table.
use rusqlite::Connection;

use super::super::super::ledger::run_store_migrations;
use super::super::*;
use crate::Store;
use crate::contracts::JobRunQuery;
use orbit_common::storage::sqlite::ledger::Migration;

/// The registry up to, not including, v38.
fn before_job_run_states() -> &'static [Migration] {
    let before = MIGRATIONS
        .iter()
        .position(|m| m.name == "job_run_states")
        .expect("v38 registered");
    &MIGRATIONS[..before]
}

/// Insert a v36-shaped run whose trailing columns sit after its state.
fn seed_run(conn: &Connection, workspace: &str, run_id: &str, state: &str, payload: Option<&str>) {
    conn.execute(
        "INSERT INTO job_runs(run_id, workspace_id, job_id, attempt, state, scheduled_at, \
         created_at, input_json, pipeline_state_json, crew_model, executed_on_json) \
         VALUES (?1, ?2, 'task_pr_pipeline', 1, ?3, '2026-10-07T00:00:00+00:00', \
         '2026-10-07T00:00:00+00:00', '{}', ?4, 'claude-opus', \
         '{\"machine_id\":\"hm_1\",\"workspace_id\":\"ws_a\"}')",
        rusqlite::params![run_id, workspace, state, payload],
    )
    .expect("seed v36 run");
}

/// `(workspace, run, bytes, storage class)` of every stored state.
type StoredState = (String, String, Vec<u8>, String);

fn stored_states(conn: &Connection, table: &str) -> Vec<StoredState> {
    let mut statement = conn
        .prepare(&format!(
            "SELECT workspace_id, run_id, CAST(pipeline_state_json AS BLOB), \
             typeof(pipeline_state_json) FROM {table} \
             WHERE pipeline_state_json IS NOT NULL ORDER BY workspace_id, run_id"
        ))
        .expect("prepare state read");
    statement
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .expect("read states")
        .collect::<Result<Vec<_>, _>>()
        .expect("collect states")
}

/// The move preserves null, small and multi-megabyte states byte for byte
/// across batches, and an interruption after earlier batches moved leaves v37
/// intact for the next open to move again.
#[test]
fn job_run_states_move_survives_interruption_byte_for_byte() {
    let dir = tempfile::tempdir().expect("fixture dir");
    let path = dir.path().join("orbit.db");
    let conn = Connection::open(&path).expect("open database");
    run_store_migrations(&conn, before_job_run_states(), None).expect("migrate to v37");
    let big = format!("{{\"blob\":\"{}\"}}", "\u{e9}x".repeat(700_000));
    seed_run(&conn, "ws_a", "jrun-null", "pending", None);
    seed_run(&conn, "ws_a", "jrun-small", "success", Some("{\"a\":1}"));
    // Enough runs that the move takes several batches.
    for index in 0..150 {
        let state = format!("{{\"index\":{index}}}");
        seed_run(
            &conn,
            "ws_a",
            &format!("jrun-{index:03}"),
            "success",
            Some(&state),
        );
    }
    // Same run id in another workspace: states are keyed by both.
    seed_run(&conn, "ws_b", "jrun-small", "failed", Some("{\"b\":2}"));
    seed_run(&conn, "ws_a", "jrun-big", "running", Some(&big));
    let before = stored_states(&conn, "job_runs");
    assert!(
        before
            .iter()
            .any(|(_, run, bytes, _)| run == "jrun-big" && bytes.len() > 1_000_000)
    );

    // Interrupt the move at the last run, after earlier batches moved.
    conn.execute_batch(
        "CREATE TRIGGER interrupt_v38 BEFORE UPDATE OF pipeline_state_json ON job_runs \
         WHEN OLD.run_id = 'jrun-big' BEGIN SELECT RAISE(ABORT, 'interrupted'); END;",
    )
    .expect("install interruption");
    run_store_migrations(&conn, MIGRATIONS, None).expect_err("interrupted migration fails");
    assert_eq!(current_schema_version(&conn).expect("version"), 37);
    assert!(!table_exists(&conn, "job_run_states").expect("table probe"));
    assert_eq!(
        stored_states(&conn, "job_runs"),
        before,
        "an interrupted move must leave every state in job_runs"
    );

    conn.execute_batch("DROP TRIGGER interrupt_v38;")
        .expect("remove interruption");
    run_store_migrations(&conn, MIGRATIONS, None).expect("rerun v38");
    assert_eq!(
        current_schema_version(&conn).expect("version"),
        SUPPORTED_SCHEMA_VERSION
    );
    assert!(!table_has_column(&conn, "job_runs", "pipeline_state_json").expect("column probe"));
    assert_eq!(stored_states(&conn, "job_run_states"), before);
    let null_rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM job_run_states WHERE run_id = 'jrun-null'",
            [],
            |row| row.get(0),
        )
        .expect("count null state");
    assert_eq!(null_rows, 0, "a run without state gets no state row");

    // The run listing still decodes every moved run, trailing columns included.
    drop(conn);
    let store = Store::open_read_only(&path).expect("open migrated database");
    let runs = store
        .read()
        .expect("read migrated database")
        .job_run()
        .list("ws_a", &JobRunQuery::default())
        .expect("list moved runs");
    assert_eq!(runs.len(), 153);
    assert!(
        runs.iter()
            .all(|run| run.crew_model.as_deref() == Some("claude-opus"))
    );
}
