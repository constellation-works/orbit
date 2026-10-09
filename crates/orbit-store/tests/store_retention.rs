//! Bounded retention through the public retention contract: audit rows and
//! terminal run state older than a cutoff go in short batches, everything
//! else stays.

#![allow(clippy::expect_used, clippy::unwrap_used, missing_docs)]

use std::process::Command;
use std::time::{Duration, Instant};

use chrono::{DateTime, TimeDelta, TimeZone, Utc};
use orbit_common::{process, test_env};
use orbit_store::Store;
use orbit_store::compose::workspace_job_run_store;
use orbit_store::contracts::{
    AuditRetentionTable, JobRunStepParams, RetentionSelection, StoreRetentionBackend,
};
use orbit_types::workflow::{JobRunState, JobTargetType, PipelineState};
use serde_json::json;

const CHILD: &str = "ORBIT_STORE_RETENTION_CHILD";

/// Run `test` again in a child process rooted in a fresh directory, or return
/// that directory when already inside the child.
fn isolated(test: &str, timeout: Duration) -> Option<std::path::PathBuf> {
    if std::env::var(CHILD).as_deref() == Ok(test) {
        return Some(std::env::current_dir().unwrap());
    }
    let root = tempfile::tempdir_in(test_env::canonical_temp_dir()).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    test_env::clear_inherited_authority(|key| {
        command.env_remove(key);
    });
    command
        .args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env(CHILD, test)
        .env("HOME", root.path())
        .env("USERPROFILE", root.path())
        .current_dir(root.path());
    let output = process::run_bounded_capped(&mut command, timeout, 256 * 1024).unwrap();
    test_env::assert_child_test_passed(test, output.status, output.stdout, output.stderr);
    None
}

fn count(store: &Store, sql: &str) -> i64 {
    store
        .connection()
        .lock()
        .unwrap()
        .query_row(sql, [], |row| row.get(0))
        .expect(sql)
}

fn insert_command_audit(store: &Store, execution_id: &str, at: DateTime<Utc>) {
    store
        .connection()
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO audit_events (execution_id, timestamp, command, role, status, \
                 exit_code, duration_ms, working_directory, pid) \
             VALUES (?1, ?2, 'task', 'codex', 'success', 0, 1, '.', 1)",
            rusqlite::params![execution_id, at.to_rfc3339()],
        )
        .unwrap();
}

/// Insert `rows` run-audit rows for `workspace_id`, one second apart from
/// `first`, in a single statement.
fn insert_run_audit(store: &Store, workspace_id: &str, first: DateTime<Utc>, rows: i64) {
    store
        .connection()
        .lock()
        .unwrap()
        .execute(
            "WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM n WHERE i + 1 < ?3) \
             INSERT INTO v2_audit_events (workspace_id, event_id, source, schema_version, \
                 event_type, ts, run_id, agent_identity, payload_json) \
             SELECT ?1, 'evt-' || i, 'loop_event', 1, 'tool.call.result', \
                 strftime('%Y-%m-%dT%H:%M:%S+00:00', ?2, '+' || i || ' seconds'), \
                 'jrun-' || (i / 100), 'codex', \
                 '{\"type\":\"tool.call.result\",\"output_ref\":\"' || i || '\"}' \
             FROM n",
            rusqlite::params![
                workspace_id,
                first.format("%Y-%m-%d %H:%M:%S").to_string(),
                rows
            ],
        )
        .unwrap();
}

fn drain(mut batch: impl FnMut() -> usize) -> (usize, Duration) {
    let mut removed = 0;
    let mut slowest = Duration::ZERO;
    loop {
        let started = Instant::now();
        let n = batch();
        slowest = slowest.max(started.elapsed());
        if n == 0 {
            return (removed, slowest);
        }
        removed += n;
    }
}

#[test]
fn audit_retention_removes_only_rows_past_the_cutoff() {
    const TEST: &str = "audit_retention_removes_only_rows_past_the_cutoff";
    let Some(root) = isolated(TEST, Duration::from_secs(60)) else {
        return;
    };
    let store = Store::open(&root.join("orbit.db")).unwrap();
    let cutoff = Utc.with_ymd_and_hms(2026, 9, 1, 0, 0, 0).unwrap();
    for day in 0..4 {
        insert_command_audit(
            &store,
            &format!("old-{day}"),
            cutoff - TimeDelta::days(day + 1),
        );
        insert_command_audit(&store, &format!("new-{day}"), cutoff + TimeDelta::days(day));
    }
    insert_run_audit(&store, "ws_a", cutoff - TimeDelta::seconds(250), 500);
    insert_run_audit(&store, "ws_b", cutoff - TimeDelta::days(30), 40);

    let command = store
        .audit_retention_selection(AuditRetentionTable::Command, "ws_a", cutoff)
        .unwrap();
    assert_eq!(command.rows, 4);
    assert!(command.bytes > 0);
    let run = store
        .audit_retention_selection(AuditRetentionTable::Run, "ws_a", cutoff)
        .unwrap();
    assert_eq!(
        run.rows, 250,
        "only this workspace's rows before the cutoff"
    );
    assert!(run.bytes > 0);

    // Planning wrote nothing.
    assert_eq!(count(&store, "SELECT COUNT(*) FROM audit_events"), 8);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM v2_audit_events"), 540);

    let (removed, _) = drain(|| {
        store
            .prune_audit_retention_batch(AuditRetentionTable::Command, "ws_a", cutoff, 3)
            .unwrap()
    });
    assert_eq!(removed, 4);
    let (removed, _) = drain(|| {
        store
            .prune_audit_retention_batch(AuditRetentionTable::Run, "ws_a", cutoff, 64)
            .unwrap()
    });
    assert_eq!(removed, 250);

    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) FROM audit_events WHERE execution_id LIKE 'new-%'"
        ),
        4
    );
    assert_eq!(count(&store, "SELECT COUNT(*) FROM audit_events"), 4);
    assert_eq!(
        count(
            &store,
            &format!(
                "SELECT COUNT(*) FROM v2_audit_events WHERE workspace_id = 'ws_a' AND ts < '{}'",
                cutoff.to_rfc3339()
            )
        ),
        0
    );
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) FROM v2_audit_events WHERE workspace_id = 'ws_a'"
        ),
        250
    );
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) FROM v2_audit_events WHERE workspace_id = 'ws_b'"
        ),
        40,
        "another workspace's run audit is out of scope"
    );
    for table in [AuditRetentionTable::Command, AuditRetentionTable::Run] {
        assert_eq!(
            store
                .audit_retention_selection(table, "ws_a", cutoff)
                .unwrap(),
            RetentionSelection::default()
        );
    }
    let space = store.store_space().unwrap();
    assert!(space.page_size > 0 && space.page_count > 0);
    assert!(
        space.freelist_pages > 0,
        "deleted rows leave reusable pages"
    );
}

#[test]
fn run_retention_drops_terminal_state_and_keeps_runs_and_steps() {
    const TEST: &str = "run_retention_drops_terminal_state_and_keeps_runs_and_steps";
    let Some(root) = isolated(TEST, Duration::from_secs(60)) else {
        return;
    };
    let store = Store::open(&root.join("orbit.db")).unwrap();
    let jobs = workspace_job_run_store(store.clone(), "ws_a");
    let cutoff = Utc.with_ymd_and_hms(2026, 9, 1, 0, 0, 0).unwrap();
    let old = cutoff - TimeDelta::days(10);

    let mut runs = Vec::new();
    for (name, state, finished) in [
        ("old-success", Some(JobRunState::Success), old),
        ("old-failed", Some(JobRunState::Failed), old),
        ("old-held", Some(JobRunState::Held), old),
        ("old-running", None, old),
        (
            "fresh-success",
            Some(JobRunState::Success),
            cutoff + TimeDelta::days(1),
        ),
    ] {
        let run = jobs.insert_job_run(name, 1, old, None, None).unwrap();
        let state_doc = PipelineState::new(
            run.run_id.clone(),
            run.job_id.clone(),
            json!({ "payload": "x".repeat(4096) }),
        );
        assert!(jobs.initialize_run_state(&run.run_id, &state_doc).unwrap());
        jobs.mark_job_run_running(&run.run_id, old, std::process::id())
            .unwrap();
        jobs.complete_job_run_step(
            &run.run_id,
            &JobRunStepParams {
                step_index: 0,
                target_type: JobTargetType::Activity,
                target_id: "implement".into(),
                started_at: old,
                finished_at: finished,
                duration_ms: Some(1),
                exit_code: Some(0),
                agent_response_json: Some(json!({ "summary": "done" })),
                state: JobRunState::Success,
                error_code: None,
                error_message: None,
            },
        )
        .unwrap();
        if let Some(state) = state {
            jobs.finalize_job_run(&run.run_id, state, finished, Some(1))
                .unwrap();
        }
        runs.push((name, run.run_id));
    }
    let other = workspace_job_run_store(store.clone(), "ws_b");
    let foreign = other.insert_job_run("foreign", 1, old, None, None).unwrap();
    other
        .initialize_run_state(
            &foreign.run_id,
            &PipelineState::new(foreign.run_id.clone(), "foreign".into(), json!({})),
        )
        .unwrap();
    other
        .mark_job_run_running(&foreign.run_id, old, std::process::id())
        .unwrap();
    other
        .finalize_job_run(&foreign.run_id, JobRunState::Success, old, Some(1))
        .unwrap();

    let plan = store.run_state_retention_selection("ws_a", cutoff).unwrap();
    assert_eq!(plan.rows, 2, "old success and old failed only");
    assert!(plan.bytes >= 2 * 4096);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM job_run_states"), 6);

    let archived_at = Utc::now();
    let (archived, _) = drain(|| {
        store
            .archive_run_states_batch("ws_a", cutoff, archived_at, 1)
            .unwrap()
    });
    assert_eq!(archived, 2);

    for (name, run_id) in &runs {
        let run = jobs.get_job_run(run_id).unwrap().expect("run row kept");
        assert_eq!(run.steps.len(), 1, "{name} keeps its steps");
        let state = jobs.read_run_state(run_id).unwrap();
        let archived = count(
            &store,
            &format!(
                "SELECT COUNT(*) FROM job_runs WHERE run_id = '{run_id}' AND archived_at IS NOT NULL"
            ),
        );
        if matches!(*name, "old-success" | "old-failed") {
            assert!(state.is_none(), "{name} pipeline state dropped");
            assert_eq!(archived, 1, "{name} stamped archived");
        } else {
            assert!(state.is_some(), "{name} pipeline state kept");
            assert_eq!(archived, 0, "{name} untouched");
        }
    }
    assert!(other.read_run_state(&foreign.run_id).unwrap().is_some());
    assert_eq!(
        store.run_state_retention_selection("ws_a", cutoff).unwrap(),
        RetentionSelection::default()
    );
}

/// The retention batch size the runtime applies.
const BATCH_ROWS: usize = 1_000;

#[test]
fn million_row_run_audit_prunes_in_sub_second_batches() {
    const TEST: &str = "million_row_run_audit_prunes_in_sub_second_batches";
    let Some(root) = isolated(TEST, Duration::from_secs(600)) else {
        return;
    };
    let store = Store::open(&root.join("orbit.db")).unwrap();
    let cutoff = Utc.with_ymd_and_hms(2026, 9, 1, 0, 0, 0).unwrap();
    // Half the history predates the cutoff.
    insert_run_audit(
        &store,
        "ws_a",
        cutoff - TimeDelta::seconds(500_000),
        1_000_000,
    );
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM v2_audit_events"),
        1_000_000
    );

    let (removed, slowest) = drain(|| {
        store
            .prune_audit_retention_batch(AuditRetentionTable::Run, "ws_a", cutoff, BATCH_ROWS)
            .unwrap()
    });
    assert_eq!(removed, 500_000);
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM v2_audit_events"),
        500_000
    );
    assert!(
        slowest < Duration::from_secs(1),
        "one {BATCH_ROWS}-row retention batch held the write lock for {slowest:?}"
    );
}
