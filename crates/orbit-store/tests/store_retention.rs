//! Bounded retention through the public retention contract: audit rows,
//! terminal run state, and applied task-commit journal rows older than a
//! cutoff go in short batches, and everything recovery still needs stays.

#![allow(clippy::expect_used, clippy::unwrap_used, missing_docs)]

use std::process::Command;
use std::time::Duration;

use chrono::{DateTime, TimeDelta, TimeZone, Utc};
use orbit_common::{process, test_env};
use orbit_store::Store;
use orbit_store::compose::{workspace_coordinated_backends, workspace_job_run_store};
use orbit_store::contracts::{
    AuditRetentionTable, JobRunStepParams, RetentionSelection, StoreRetentionBackend,
    TaskCoordinationCommitOutcome, TaskCoordinationCommitParams, TaskCreateParams,
};
use orbit_store::maintenance::task_registry::{
    BindWorkspaceParams, TaskRegistryStore, task_registry_path,
};
use orbit_types::task::{TaskComplexity, TaskPriority, TaskStatus, TaskType};
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

/// The calling thread's CPU time. A saturated host stretches wall-clock time
/// without changing the work a batch does, so a batch's cost is read here.
#[cfg(unix)]
fn thread_cpu_time() -> Duration {
    let mut time = std::mem::MaybeUninit::<libc::timespec>::zeroed();
    // Safety: `clock_gettime` fills the `timespec` it is handed and reads nothing else.
    assert_eq!(
        unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, time.as_mut_ptr()) },
        0
    );
    // Safety: a zero return means the kernel filled `time`.
    let time = unsafe { time.assume_init() };
    Duration::new(time.tv_sec as u64, time.tv_nsec as u32)
}

#[cfg(not(unix))]
fn thread_cpu_time() -> Duration {
    static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    START.get_or_init(std::time::Instant::now).elapsed()
}

/// Run `batch` until it removes nothing. Returns the rows removed and the
/// slowest batch's thread CPU time.
fn drain(mut batch: impl FnMut() -> usize) -> (usize, Duration) {
    let mut removed = 0;
    let mut slowest = Duration::ZERO;
    loop {
        let started = thread_cpu_time();
        let n = batch();
        slowest = slowest.max(thread_cpu_time() - started);
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
                lock_wait_ms: None,
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
        "one {BATCH_ROWS}-row retention batch held the write lock for {slowest:?} of CPU"
    );
}

fn journal_ids(store: &Store, sql: &str) -> Vec<String> {
    let conn = store.connection();
    let conn = conn.lock().unwrap();
    let mut stmt = conn.prepare(sql).unwrap();
    stmt.query_map([], |row| row.get(0))
        .unwrap()
        .map(|row| row.unwrap())
        .collect()
}

fn journal_state(store: &Store, journal_id: &str) -> String {
    store
        .connection()
        .lock()
        .unwrap()
        .query_row(
            "SELECT state FROM task_commit_journal WHERE journal_id = ?1",
            rusqlite::params![journal_id],
            |row| row.get(0),
        )
        .unwrap()
}

/// Applied rows older than the cutoff go when a newer applied row of the same
/// task exists. The newest applied row stays even when it is old, and so do
/// prepared, committed and aborted rows. Recovery then still rolls the kept
/// committed row forward.
#[test]
fn journal_retention_keeps_the_newest_applied_row_and_recovery_still_replays() {
    const TEST: &str = "journal_retention_keeps_the_newest_applied_row_and_recovery_still_replays";
    let Some(root) = isolated(TEST, Duration::from_secs(120)) else {
        return;
    };
    let partition = "orbit-test-123456";
    let registry = TaskRegistryStore::open(&task_registry_path(&root)).unwrap();
    let repo = root.join(partition);
    let orbit_dir = repo.join(".orbit");
    std::fs::create_dir_all(&orbit_dir).unwrap();
    let binding = registry
        .bind_workspace(BindWorkspaceParams {
            partition_id: Some(partition.to_string()),
            slug: "Orbit Test".to_string(),
            repo_root: repo.clone(),
            workspace_path: repo,
            orbit_dir,
            repo_fingerprint: None,
        })
        .unwrap();
    let store = Store::open(&root.join("state.sqlite")).unwrap();
    let backends =
        workspace_coordinated_backends(registry, binding.partition_id.clone(), store.clone())
            .unwrap();
    let task = backends
        .task
        .task
        .create_task(TaskCreateParams {
            actor: "codex".to_string(),
            parent_id: None,
            title: "Journal retention".to_string(),
            description: "Detailed task description".to_string(),
            acceptance_criteria: vec!["First criterion".to_string()],
            dependencies: Vec::new(),
            relations: Vec::new(),
            tags: Vec::new(),
            required_tools: Vec::new(),
            plan: "1. Do the work".to_string(),
            execution_summary: String::new(),
            context_files: vec!["src/lib.rs".to_string()],
            repo_root: None,
            created_by: Some("codex".to_string()),
            planned_by: None,
            implemented_by: None,
            status: TaskStatus::Backlog,
            priority: TaskPriority::High,
            complexity: Some(TaskComplexity::Medium),
            task_type: TaskType::Feature,
            external_refs: Vec::new(),
            source_task_id: None,
            crew: None,
            crew_source: None,
            orchestrator: None,
            comments: Vec::new(),
            context_creation: Vec::new(),
        })
        .unwrap();
    let outcome = backends
        .commit_boundary
        .commit_task_transition(&TaskCoordinationCommitParams {
            task_id: task.id.clone(),
            actor: "codex".into(),
            expected_status: vec![TaskStatus::Backlog],
            status: Some(TaskStatus::InProgress),
            ..Default::default()
        })
        .unwrap();
    assert!(
        matches!(outcome, TaskCoordinationCommitOutcome::Committed(_)),
        "the transition must settle so its journal row is applied"
    );

    let applied = journal_ids(
        &store,
        &format!(
            "SELECT journal_id FROM task_commit_journal \
             WHERE workspace_id = '{}' AND task_id = '{}' AND state = 'applied' \
             ORDER BY created_at, journal_id",
            binding.partition_id, task.id
        ),
    );
    assert!(
        !applied.is_empty(),
        "the status transition leaves an applied journal row, got {applied:?}"
    );
    let newest = applied.last().unwrap().clone();
    let older_real = &applied[..applied.len() - 1];
    let replay_intent: String = store
        .connection()
        .lock()
        .unwrap()
        .query_row(
            "SELECT intent_json FROM task_commit_journal WHERE journal_id = ?1",
            rusqlite::params![newest],
            |row| row.get(0),
        )
        .unwrap();
    let age = |id: &str, at: &str| {
        store
            .connection()
            .lock()
            .unwrap()
            .execute(
                "UPDATE task_commit_journal SET created_at = ?2, applied_at = ?2 \
                 WHERE journal_id = ?1",
                rusqlite::params![id, at],
            )
            .unwrap();
    };
    age(&newest, "2025-12-01T00:00:00+00:00");
    for id in older_real {
        age(id, "2025-01-01T00:00:00+00:00");
    }
    let insert = |id: &str,
                  workspace: &str,
                  task_id: &str,
                  state: &str,
                  intent: &str,
                  at: &str,
                  applied_at: Option<&str>| {
        store
            .connection()
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO task_commit_journal ( \
                     journal_id, workspace_id, task_id, state, intent_json, \
                     created_at, committed_at, applied_at, reservation_id \
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7, NULL)",
                rusqlite::params![id, workspace, task_id, state, intent, at, applied_at],
            )
            .unwrap();
    };
    let workspace = binding.partition_id.as_str();
    insert(
        "old-dup",
        workspace,
        &task.id,
        "applied",
        "older-duplicate",
        "2024-01-01T00:00:00+00:00",
        Some("2024-01-01T00:00:00+00:00"),
    );
    // Same timestamp: the higher journal id is the newest and stays.
    insert(
        "tie-a",
        workspace,
        "ORB-90002",
        "applied",
        "tie-dropped",
        "2025-06-01T00:00:00+00:00",
        Some("2025-06-01T00:00:00+00:00"),
    );
    insert(
        "tie-b",
        workspace,
        "ORB-90002",
        "applied",
        "tie-kept",
        "2025-06-01T00:00:00+00:00",
        Some("2025-06-01T00:00:00+00:00"),
    );
    insert(
        "recent",
        workspace,
        "ORB-90003",
        "applied",
        "inside-window",
        "2026-06-01T00:00:00+00:00",
        Some("2026-06-01T00:00:00+00:00"),
    );
    insert(
        "still-prepared",
        workspace,
        &task.id,
        "prepared",
        "{}",
        "2020-01-01T00:00:00+00:00",
        None,
    );
    insert(
        "still-committed",
        workspace,
        &task.id,
        "committed",
        &replay_intent,
        "2020-02-01T00:00:00+00:00",
        None,
    );
    insert(
        "still-aborted",
        workspace,
        &task.id,
        "aborted",
        "{}",
        "2020-03-01T00:00:00+00:00",
        None,
    );
    insert(
        "other-old",
        "ws-other",
        "ORB-90004",
        "applied",
        "other-workspace-old",
        "2024-01-01T00:00:00+00:00",
        Some("2024-01-01T00:00:00+00:00"),
    );
    insert(
        "other-newer",
        "ws-other",
        "ORB-90004",
        "applied",
        "other-workspace-newer",
        "2024-06-01T00:00:00+00:00",
        Some("2024-06-01T00:00:00+00:00"),
    );

    let cutoff = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    let mut expected_drop = older_real.to_vec();
    expected_drop.extend(["old-dup".to_string(), "tie-a".to_string()]);
    let before = count(&store, "SELECT COUNT(*) FROM task_commit_journal");
    let selection = store
        .journal_retention_selection(workspace, cutoff)
        .unwrap();
    assert_eq!(selection.rows, expected_drop.len() as u64);
    let expected_bytes: i64 = store
        .connection()
        .lock()
        .unwrap()
        .query_row(
            &format!(
                "SELECT COALESCE(SUM(octet_length(intent_json)), 0) FROM task_commit_journal \
                 WHERE journal_id IN ({})",
                expected_drop
                    .iter()
                    .map(|id| format!("'{id}'"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(selection.bytes, expected_bytes as u64);
    assert!(selection.bytes > 0, "reclaimable intent bytes are reported");
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM task_commit_journal"),
        before,
        "a plan deletes nothing"
    );

    let (removed, _) = drain(|| {
        store
            .prune_journal_retention_batch(workspace, cutoff, 1)
            .unwrap()
    });
    assert_eq!(removed, expected_drop.len());
    for id in &expected_drop {
        assert!(
            journal_ids(
                &store,
                &format!("SELECT journal_id FROM task_commit_journal WHERE journal_id = '{id}'")
            )
            .is_empty(),
            "{id} is an older applied row and should be gone"
        );
    }
    for id in [
        "tie-b",
        "recent",
        "other-old",
        "other-newer",
        "still-prepared",
        "still-committed",
        "still-aborted",
    ] {
        assert_eq!(
            journal_ids(
                &store,
                &format!("SELECT journal_id FROM task_commit_journal WHERE journal_id = '{id}'")
            )
            .len(),
            1,
            "{id} stays"
        );
    }
    assert_eq!(journal_state(&store, &newest), "applied");
    assert_eq!(
        store
            .journal_retention_selection(workspace, cutoff)
            .unwrap(),
        RetentionSelection::default(),
        "a second pass finds nothing left to drop"
    );

    backends.commit_boundary.recover().unwrap();
    assert_eq!(
        journal_state(&store, "still-committed"),
        "applied",
        "recovery replays the committed row that retention kept"
    );
    assert_eq!(
        journal_state(&store, "still-prepared"),
        "aborted",
        "recovery abandons the prepared row that retention kept"
    );
    assert_eq!(journal_state(&store, "still-aborted"), "aborted");
    assert_eq!(journal_state(&store, &newest), "applied");
    assert_eq!(
        backends
            .task
            .task
            .get_task(&task.id)
            .unwrap()
            .unwrap()
            .status,
        TaskStatus::InProgress,
        "replaying the kept committed intent leaves the published transition in place"
    );
}
