// v38 `job_run_states`: pipeline state leaves `job_runs` for a side table.
use std::time::{Duration, Instant};

use orbit_types::workflow::JobRunState;
use rusqlite::Connection;

use super::super::ledger::{self, Migration};
use super::super::*;
use crate::Store;
use crate::compose::workspace_job_run_store;
use crate::contracts::JobRunQuery;

/// The registry up to, not including, v38.
fn before_job_run_states() -> &'static [Migration] {
    let before = ledger::MIGRATIONS
        .iter()
        .position(|m| m.name == "job_run_states")
        .expect("v38 registered");
    &ledger::MIGRATIONS[..before]
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
    ledger::run_migrations(&conn, before_job_run_states()).expect("migrate to v37");
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
    ledger::run_migrations(&conn, ledger::MIGRATIONS).expect_err("interrupted migration fails");
    assert_eq!(current_schema_version(&conn).expect("version"), 37);
    assert!(!table_exists(&conn, "job_run_states").expect("table probe"));
    assert_eq!(
        stored_states(&conn, "job_runs"),
        before,
        "an interrupted move must leave every state in job_runs"
    );

    conn.execute_batch("DROP TRIGGER interrupt_v38;")
        .expect("remove interruption");
    ledger::run_migrations(&conn, ledger::MIGRATIONS).expect("rerun v38");
    assert_eq!(
        current_schema_version(&conn).expect("version"),
        ledger::SUPPORTED_SCHEMA_VERSION
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
    let runs = workspace_job_run_store(store, "ws_a")
        .list_job_runs_filtered(&JobRunQuery::default())
        .expect("list moved runs");
    assert_eq!(runs.len(), 153);
    assert!(
        runs.iter()
            .all(|run| run.crew_model.as_deref() == Some("claude-opus"))
    );
}

/// Runs in the listing fixture and their mean pipeline-state size.
const BENCH_RUNS: usize = 20_000;
const BENCH_MEAN_STATE_BYTES: usize = 90 * 1024;

/// Best of four listings of every `success` run in `ws_a`; the first warms
/// the page cache.
fn time_success_listing(path: &std::path::Path) -> (Duration, usize) {
    let store = Store::open_read_only(path).expect("open fixture");
    let jobs = workspace_job_run_store(store, "ws_a");
    let query = JobRunQuery {
        state: Some(JobRunState::Success),
        ..Default::default()
    };
    let mut best = Duration::MAX;
    let mut listed = 0;
    for _ in 0..4 {
        let started = Instant::now();
        listed = jobs.list_job_runs_filtered(&query).expect("list").len();
        best = best.min(started.elapsed());
    }
    (best, listed)
}

/// Bench: listing 20k runs whose state averages 90 KB, before and
/// after v38, plus the migration's own duration and leftover freelist.
///
/// Run with `cargo test -p orbit-store --lib job_run_states_listing_bench --
/// --ignored --nocapture`; it writes a 1.8 GB fixture under the system temp
/// directory (`TMPDIR`). Measured 2026-10-07 on the Linux build host (SSD,
/// warm page cache), listing 18,000 success runs: 517 ms before the state move
/// and 84 ms after; the migration took 32 s and left a 5.8 MB freelist. The
/// fixture now measures from schema v37 to v38 after the ledger was rebased.
#[test]
#[ignore = "bench: writes a 1.8 GB fixture; run with --ignored --nocapture"]
#[allow(clippy::print_stderr)]
fn job_run_states_listing_bench() {
    let dir = tempfile::tempdir().expect("bench dir");
    let path = dir.path().join("orbit.db");
    {
        let conn = Connection::open(&path).expect("open fixture");
        orbit_common::storage::sqlite::apply_default_pragmas(&conn).expect("pragmas");
        ledger::run_migrations(&conn, before_job_run_states()).expect("migrate to v37");
        let tx = conn.unchecked_transaction().expect("seed transaction");
        for index in 0..BENCH_RUNS {
            // 30..=150 KB in 10 KB steps: a 90 KB mean.
            let size = (30 + (index % 13) * 10) * 1024;
            let state = format!("{{\"pad\":\"{}\"}}", "x".repeat(size));
            let run_state = if index % 10 == 0 { "failed" } else { "success" };
            seed_run(
                &tx,
                "ws_a",
                &format!("jrun-{index:05}"),
                run_state,
                Some(&state),
            );
        }
        tx.commit().expect("commit fixture");
    }
    assert_eq!(BENCH_MEAN_STATE_BYTES, (30 + 6 * 10) * 1024);

    let (before, listed_before) = time_success_listing(&path);
    let started = Instant::now();
    drop(Store::open(&path).expect("apply v38"));
    let migration = started.elapsed();
    let freelist_bytes = {
        let conn = Connection::open(&path).expect("reopen fixture");
        let pages: i64 = conn
            .query_row("PRAGMA freelist_count", [], |row| row.get(0))
            .expect("freelist");
        let page_size: i64 = conn
            .query_row("PRAGMA page_size", [], |row| row.get(0))
            .expect("page size");
        pages * page_size
    };
    let (after, listed_after) = time_success_listing(&path);

    eprintln!(
        "job_run_states bench: {BENCH_RUNS} runs, listed {listed_after} success runs; \
         before v38 {before:?}, after v38 {after:?}; migration {migration:?}; \
         freelist {freelist_bytes} bytes"
    );
    assert_eq!(listed_before, listed_after);
    assert!(
        after < Duration::from_millis(300),
        "listing every success run took {after:?} after v38 (before: {before:?})"
    );
}
