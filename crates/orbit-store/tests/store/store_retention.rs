//  Bounded retention through the public retention contract: audit rows and
//  terminal run state older than a cutoff go in short batches, everything
//  else stays.

const CHILD: &str = "ORBIT_STORE_RETENTION_CHILD";

/// Run `test` again in a child process rooted in a fresh directory, or return
/// that directory when already inside the child.
fn store_retention_isolated(test: &str, timeout: Duration) -> Option<std::path::PathBuf> {
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

fn workspace_only(id: &str) -> RetentionWorkspaceScope<'_> {
    RetentionWorkspaceScope {
        workspace_id: id,
        legacy_ids: None,
    }
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
    let Some(root) = store_retention_isolated(TEST, Duration::from_secs(60)) else {
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
        .audit_retention_selection(AuditRetentionTable::Command, workspace_only("ws_a"), cutoff)
        .unwrap();
    assert_eq!(command.rows, 4);
    assert!(command.bytes > 0);
    let run = store
        .audit_retention_selection(AuditRetentionTable::Run, workspace_only("ws_a"), cutoff)
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
            .prune_audit_retention_batch(
                AuditRetentionTable::Command,
                workspace_only("ws_a"),
                cutoff,
                3,
            )
            .unwrap()
    });
    assert_eq!(removed, 4);
    let (removed, _) = drain(|| {
        store
            .prune_audit_retention_batch(
                AuditRetentionTable::Run,
                workspace_only("ws_a"),
                cutoff,
                64,
            )
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
                .audit_retention_selection(table, workspace_only("ws_a"), cutoff)
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
    let Some(root) = store_retention_isolated(TEST, Duration::from_secs(60)) else {
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

    let plan = store
        .run_state_retention_selection(workspace_only("ws_a"), cutoff)
        .unwrap();
    assert_eq!(plan.rows, 2, "old success and old failed only");
    assert!(plan.bytes >= 2 * 4096);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM job_run_states"), 6);

    let archived_at = Utc::now();
    let (archived, _) = drain(|| {
        store
            .archive_run_states_batch(workspace_only("ws_a"), cutoff, archived_at, 1)
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
        store
            .run_state_retention_selection(workspace_only("ws_a"), cutoff)
            .unwrap(),
        RetentionSelection::default()
    );
}

/// The retention batch size the runtime applies.
const BATCH_ROWS: usize = 1_000;

#[test]
fn million_row_run_audit_prunes_in_sub_second_batches() {
    const TEST: &str = "million_row_run_audit_prunes_in_sub_second_batches";
    let Some(root) = store_retention_isolated(TEST, Duration::from_secs(600)) else {
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
            .prune_audit_retention_batch(
                AuditRetentionTable::Run,
                workspace_only("ws_a"),
                cutoff,
                BATCH_ROWS,
            )
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

fn insert_journal_row(
    store: &Store,
    workspace_id: &str,
    journal_id: &str,
    task_id: &str,
    state: &str,
    created_at: DateTime<Utc>,
) {
    let applied_at =
        (state == "applied").then(|| (created_at + TimeDelta::seconds(1)).to_rfc3339());
    store
        .connection()
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO task_commit_journal (journal_id, workspace_id, task_id, state, \
                 intent_json, created_at, committed_at, applied_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, ?7)",
            rusqlite::params![
                journal_id,
                workspace_id,
                task_id,
                state,
                format!(
                    "{{\"task_id\":\"{task_id}\",\"pad\":\"{}\"}}",
                    "x".repeat(512)
                ),
                created_at.to_rfc3339(),
                applied_at
            ],
        )
        .unwrap();
}

fn journal_ids(store: &Store, sql: &str) -> Vec<String> {
    let connection = store.connection();
    let conn = connection.lock().unwrap();
    let mut stmt = conn.prepare(sql).unwrap();
    stmt.query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

/// Applied journal rows settled before the cutoff go, oldest first, in
/// batches by key. Each task keeps its newest applied row, and prepared,
/// committed and aborted rows stay however old they are. Another registered
/// workspace is out of scope; a legacy partition the caller names is in.
#[test]
fn journal_retention_removes_old_applied_rows_and_keeps_unsettled_and_newest() {
    const TEST: &str = "journal_retention_removes_old_applied_rows_and_keeps_unsettled_and_newest";
    let Some(root) = store_retention_isolated(TEST, Duration::from_secs(60)) else {
        return;
    };
    let store = Store::open(&root.join("orbit.db")).unwrap();
    let cutoff = Utc.with_ymd_and_hms(2026, 9, 1, 0, 0, 0).unwrap();
    let days_before = |days: i64| cutoff - TimeDelta::days(days);
    for (workspace_id, journal_id, task_id, state, created_at) in [
        // Three old applied rows: the two older go, the newest stays.
        ("ws_a", "a-t1-1", "T-1", "applied", days_before(30)),
        ("ws_a", "a-t1-2", "T-1", "applied", days_before(20)),
        ("ws_a", "a-t1-3", "T-1", "applied", days_before(10)),
        // A lone old applied row is its task's newest.
        ("ws_a", "a-t2-1", "T-2", "applied", days_before(30)),
        // Old applied rows go once a newer one exists past the cutoff, which
        // itself stays.
        ("ws_a", "a-t3-1", "T-3", "applied", days_before(30)),
        ("ws_a", "a-t3-2", "T-3", "applied", days_before(5)),
        (
            "ws_a",
            "a-t3-3",
            "T-3",
            "applied",
            cutoff + TimeDelta::days(1),
        ),
        // Unsettled and aborted rows are never selected, however old.
        ("ws_a", "a-t4-prepared", "T-4", "prepared", days_before(40)),
        (
            "ws_a",
            "a-t4-committed",
            "T-4",
            "committed",
            days_before(40),
        ),
        ("ws_a", "a-t4-aborted", "T-4", "aborted", days_before(40)),
        ("ws_a", "a-t4-1", "T-4", "applied", days_before(35)),
        ("ws_a", "a-t4-2", "T-4", "applied", days_before(34)),
        // Another registered workspace keeps its own window.
        ("ws_b", "b-t1-1", "T-1", "applied", days_before(30)),
        ("ws_b", "b-t1-2", "T-1", "applied", days_before(20)),
        // A legacy partition the caller names is pruned like this one.
        ("ws_old", "old-t1-1", "T-1", "applied", days_before(30)),
        ("ws_old", "old-t1-2", "T-1", "applied", days_before(20)),
    ] {
        insert_journal_row(&store, workspace_id, journal_id, task_id, state, created_at);
    }
    assert_eq!(
        store
            .distinct_workspace_ids(RetentionIdSource::CommitJournal)
            .unwrap()
            .len(),
        3
    );
    let legacy = vec!["ws_old".to_string()];
    let scope = RetentionWorkspaceScope {
        workspace_id: "ws_a",
        legacy_ids: Some(&legacy),
    };

    let plan = store.journal_retention_candidates(scope, cutoff).unwrap();
    assert_eq!(
        plan.journal_ids,
        ["a-t4-1", "a-t1-1", "a-t3-1", "a-t1-2", "a-t3-2", "old-t1-1"],
        "old applied rows except each task's newest, oldest first per workspace"
    );
    assert!(plan.bytes >= 6 * 512, "bytes cover the stored intents");
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM task_commit_journal"),
        16,
        "planning wrote nothing"
    );

    let mut batches = plan.journal_ids.chunks(4);
    let (removed, _) = drain(|| {
        batches
            .next()
            .map(|batch| store.delete_applied_journal_rows(batch).unwrap())
            .unwrap_or(0)
    });
    assert_eq!(removed, 6);
    assert_eq!(
        store
            .delete_applied_journal_rows(&plan.journal_ids)
            .unwrap(),
        0,
        "a repeated batch deletes nothing"
    );
    assert_eq!(
        store
            .delete_applied_journal_rows(&["a-t4-committed".to_string()])
            .unwrap(),
        0,
        "a batch never deletes a row that is not applied"
    );

    assert_eq!(
        journal_ids(
            &store,
            "SELECT journal_id FROM task_commit_journal ORDER BY journal_id"
        ),
        [
            "a-t1-3",
            "a-t2-1",
            "a-t3-3",
            "a-t4-2",
            "a-t4-aborted",
            "a-t4-committed",
            "a-t4-prepared",
            "b-t1-1",
            "b-t1-2",
            "old-t1-2",
        ]
    );
    assert_eq!(
        store.journal_retention_candidates(scope, cutoff).unwrap(),
        JournalRetentionCandidates::default()
    );
}

/// The first journal pass ranked rows with a correlated subquery per
/// candidate, which took 60 s on 40,000 rows inside its write transaction.
/// The selection must be one indexed read whose cost is a sort, and each
/// 1,000-row delete batch must stay short.
#[test]
fn forty_thousand_row_journal_selects_once_and_deletes_in_short_batches() {
    const TEST: &str = "forty_thousand_row_journal_selects_once_and_deletes_in_short_batches";
    const ROWS: i64 = 40_000;
    const TASKS: i64 = 397;
    let Some(root) = store_retention_isolated(TEST, Duration::from_secs(300)) else {
        return;
    };
    let store = Store::open(&root.join("orbit.db")).unwrap();
    let cutoff = Utc.with_ymd_and_hms(2026, 9, 1, 0, 0, 0).unwrap();
    let first = cutoff - TimeDelta::seconds(ROWS / 2);
    let state_of = |i: i64| match i % 10 {
        0 => "committed",
        1 => "prepared",
        2 => "aborted",
        _ => "applied",
    };
    store
        .connection()
        .lock()
        .unwrap()
        .execute(
            "WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM n WHERE i + 1 < ?2) \
             INSERT INTO task_commit_journal (journal_id, workspace_id, task_id, state, \
                 intent_json, created_at, committed_at, applied_at) \
             SELECT printf('commit-%06d', i), 'ws_a', 'T-' || (i % ?3), \
                 CASE i % 10 WHEN 0 THEN 'committed' WHEN 1 THEN 'prepared' \
                     WHEN 2 THEN 'aborted' ELSE 'applied' END, \
                 '{\"pad\":\"' || hex(randomblob(256)) || '\"}', \
                 strftime('%Y-%m-%dT%H:%M:%S+00:00', ?1, '+' || i || ' seconds'), NULL, \
                 CASE WHEN i % 10 > 2 THEN \
                     strftime('%Y-%m-%dT%H:%M:%S+00:00', ?1, '+' || (i + 1) || ' seconds') END \
             FROM n",
            rusqlite::params![first.format("%Y-%m-%d %H:%M:%S").to_string(), ROWS, TASKS],
        )
        .unwrap();
    // The oracle: applied rows settled before the cutoff that are not their
    // task's newest applied row.
    let mut newest = std::collections::HashMap::new();
    for i in (0..ROWS).filter(|i| state_of(*i) == "applied") {
        newest.insert(i % TASKS, i);
    }
    let expected = (0..ROWS)
        .filter(|i| state_of(*i) == "applied")
        .filter(|i| first + TimeDelta::seconds(i + 1) < cutoff)
        .filter(|i| newest[&(i % TASKS)] != *i)
        .count();
    assert!(expected > 10_000, "the fixture selects a large history");

    let started = thread_cpu_time();
    let plan = store
        .journal_retention_candidates(workspace_only("ws_a"), cutoff)
        .unwrap();
    let selection = thread_cpu_time() - started;
    assert_eq!(plan.journal_ids.len(), expected);
    assert!(
        selection < Duration::from_secs(5),
        "selecting from {ROWS} journal rows took {selection:?} of CPU"
    );

    let mut batches = plan.journal_ids.chunks(BATCH_ROWS);
    let (removed, slowest) = drain(|| {
        batches
            .next()
            .map(|batch| store.delete_applied_journal_rows(batch).unwrap())
            .unwrap_or(0)
    });
    assert_eq!(removed, expected);
    assert!(
        slowest < Duration::from_secs(1),
        "one {BATCH_ROWS}-row journal batch held the write lock for {slowest:?} of CPU"
    );
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) FROM task_commit_journal WHERE state <> 'applied'"
        ),
        ROWS * 3 / 10,
        "prepared, committed and aborted rows all stay"
    );
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(DISTINCT task_id) FROM task_commit_journal WHERE state = 'applied'"
        ),
        TASKS,
        "every task keeps an applied row"
    );
    assert!(
        store
            .journal_retention_candidates(workspace_only("ws_a"), cutoff)
            .unwrap()
            .journal_ids
            .is_empty()
    );
}
