//! Store GC contention and the timing of an orphaned, audit-silent worker.

use std::cell::RefCell;
use std::sync::mpsc::{self, SyncSender};
use std::time::Duration;

#[cfg(unix)]
use chrono::Utc;
use orbit_common::OrbitError;
use orbit_core::OrbitRuntime;
#[cfg(unix)]
use orbit_engine::RuntimeHost;
use orbit_store::Store;
#[cfg(unix)]
use orbit_store::contracts::V2AuditEventInsertParams;
#[cfg(unix)]
use orbit_types::workflow::JobRunState;
use rusqlite::Connection;
#[cfg(unix)]
use serde_json::json;

thread_local! {
    static RELEASE_WRITER: RefCell<Option<SyncSender<()>>> = const { RefCell::new(None) };
}

/// Fail the first SQLite lock wait deterministically, then release the writer.
/// Returning false guarantees the current attempt gets SQLITE_BUSY even if
/// the other thread commits before SQLite returns to the caller.
fn release_writer_on_busy(_: i32) -> bool {
    RELEASE_WRITER.with(|slot| {
        if let Some(sender) = slot.borrow_mut().take() {
            sender.send(()).unwrap();
        }
    });
    false
}

fn with_concurrent_writer<T>(
    store: &Store,
    operation: impl FnOnce() -> Result<T, OrbitError>,
) -> T {
    let connection = store.connection();
    let path = connection.lock().unwrap().path().unwrap().to_owned();
    let writer = Connection::open(path).unwrap();
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();
    let (release, released) = mpsc::sync_channel(1);
    RELEASE_WRITER.with(|slot| *slot.borrow_mut() = Some(release));
    connection
        .lock()
        .unwrap()
        .busy_handler(Some(release_writer_on_busy))
        .unwrap();
    let result = std::thread::scope(|scope| {
        let worker = scope.spawn(move || {
            released.recv_timeout(Duration::from_secs(10)).unwrap();
            writer.execute_batch("COMMIT").unwrap();
        });
        let result = operation();
        worker.join().unwrap();
        result
    });
    connection
        .lock()
        .unwrap()
        .busy_timeout(Duration::from_secs(5))
        .unwrap();
    result.expect("GC must retry a concurrent writer's lock timeout")
}

fn gc_runtime() -> (tempfile::TempDir, OrbitRuntime) {
    crate::scenarios::require_isolated_mutable_fixture("gc_runtime");
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("home/.orbit");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    assert!(
        crate::git_test_support::run(workspace.parent().unwrap(), &["init", "-q"])
            .unwrap()
            .success
    );
    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    (root, runtime)
}

fn retention_retries_busy_batches_and_keeps_live_rows() {
    let (_root, runtime) = gc_runtime();
    let store = runtime.sqlite_store().unwrap();
    let workspace = runtime.workspace_id().unwrap();
    let connection = store.connection();
    {
        let conn = connection.lock().unwrap();
        conn.execute_batch(
            "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x < 2001)
             INSERT INTO audit_events (execution_id, timestamp, command, role, status,
                 exit_code, duration_ms, working_directory, pid)
             SELECT 'old-' || x, '2020-01-01T00:00:00Z', 'task', 'codex', 'success', 0, 1, '.', 1 FROM n;
             INSERT INTO host_resource_minutes (minute_at, cpu_cores, cpu_count, memory_bytes, memory_total_bytes, samples)
             VALUES ('2020-01-01T00:00:00.000000Z', 1.0, 4, 1024, 4096, 1);"
        ).unwrap();
        conn.execute(
            "INSERT INTO job_runs (workspace_id, run_id, job_id, attempt, state, scheduled_at, created_at, finished_at)
             VALUES (?1, 'old-run', 'fixture', 1, 'success', ?2, ?2, ?2),
                    (?1, 'live-run', 'fixture', 1, 'running', ?2, ?2, NULL)",
            rusqlite::params![workspace, "2020-01-01T00:00:00Z"],
        ).unwrap();
        conn.execute(
            "INSERT INTO job_run_states (workspace_id, run_id, pipeline_state_json)
             VALUES (?1, 'old-run', '{}'), (?1, 'live-run', '{}')",
            rusqlite::params![workspace],
        )
        .unwrap();
    }
    let audit = with_concurrent_writer(&store, || runtime.gc_audit(true, Some(30)));
    assert_eq!(audit.tables[0].rows_removed, 2001);
    assert_eq!(
        audit.writes.batches, 3,
        "each audit batch removes at most 1,000 rows"
    );
    let runs = with_concurrent_writer(&store, || runtime.gc_runs(true, Some(30)));
    assert_eq!(runs.runs_archived, 1);
    assert_eq!(
        runs.resource_minutes
            .iter()
            .map(|table| table.rows_removed)
            .sum::<u64>(),
        1
    );
    let conn = connection.lock().unwrap();
    let remaining: String = conn
        .query_row("SELECT run_id FROM job_run_states", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        remaining, "live-run",
        "GC preserves a running worker's checkpoint"
    );
}

#[cfg(unix)]
fn orphan_timing_uses_exit_observation_or_detection_after_a_silent_step() {
    for exit_observed in [false, true] {
        let (_root, runtime) = gc_runtime();
        let owner = orbit_common::test_env::spawn_unrelated_process();
        let started = Utc::now() - chrono::Duration::minutes(9);
        let run = runtime
            .insert_job_run("store_gc_pipeline", 1, started, None, None)
            .unwrap();
        runtime
            .stores()
            .jobs()
            .mark_job_run_running(&run.run_id, started, owner.pid())
            .unwrap();
        let last_audit = started + chrono::Duration::milliseconds(45);
        runtime
            .insert_v2_audit_event(&V2AuditEventInsertParams {
                workspace_id: runtime.workspace_id().unwrap(),
                event_id: "started".into(),
                source: "v2_envelope".into(),
                schema_version: 1,
                event_type: "step.started".into(),
                ts: last_audit,
                run_id: run.run_id.clone(),
                agent_identity: "test".into(),
                parent_event_id: None,
                workspace_path: None,
                payload_json: json!({"event_id": "started", "ts": last_audit}).to_string(),
            })
            .unwrap();
        assert_eq!(
            runtime.show_job_run(&run.run_id).unwrap().state,
            JobRunState::Running,
            "audit silence must not interrupt a live worker"
        );
        let worker_pid = owner.pid();
        drop(owner);
        let observed_at = Utc::now() - chrono::Duration::seconds(1);
        if exit_observed {
            runtime.sqlite_store().unwrap().connection().lock().unwrap().execute(
                "INSERT INTO audit_events (execution_id, timestamp, command, role, status,
                    exit_code, duration_ms, working_directory, pid, tool_name, job_run_id, arguments_json)
                 VALUES ('exit', ?1, 'pipeline', 'test', 'success', 0, 1, '.', 1,
                     'pipeline.worker.exited', ?2, ?3)",
                rusqlite::params![observed_at.to_rfc3339(), run.run_id,
                    json!({"worker_pid": worker_pid, "exit_status": "exit status: 1", "exit_code": 1, "observed_at": observed_at}).to_string()],
            ).unwrap();
        }
        let before_reconciliation = Utc::now();
        let shown = runtime.show_job_run(&run.run_id).unwrap();
        assert_eq!(shown.state, JobRunState::Interrupted);
        assert!(
            shown.duration_ms.unwrap() >= 538_000,
            "a nine-minute silent worker must not become a 45ms run: {shown:?}"
        );
        if exit_observed {
            assert_eq!(shown.finished_at, Some(observed_at));
        } else {
            assert!(
                shown.finished_at.unwrap() >= before_reconciliation,
                "without an observed exit, finish time is detection time, an upper bound"
            );
        }
    }
}

crate::scenario_table!(
    retention_retries_busy_batches_scenarios,
    retention_retries_busy_batches_and_keeps_live_rows,
);

#[cfg(unix)]
crate::scenario_table!(
    orphan_timing_scenarios,
    orphan_timing_uses_exit_observation_or_detection_after_a_silent_step,
);
