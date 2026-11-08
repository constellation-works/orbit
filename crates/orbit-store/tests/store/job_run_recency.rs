// Per-job streak reads must not walk other jobs' retained history.
#[test]
fn per_job_recency_reads_do_not_scan_unrelated_runs() {
    use orbit_store::contracts::{JobRunOrder, JobRunQuery};
    use rusqlite::StatementStatus;
    use rusqlite::trace::{TraceEvent, TraceEventCodes};

    let store = Store::open_in_memory().unwrap();
    let jobs = workspace_job_run_store(store.clone(), "ws_recency");
    let connection = store.connection();
    // A fixed cohort, rather than concurrent traffic or a wall-clock bound,
    // makes the repeated workspace scan reproducible on any host.
    connection
        .lock()
        .unwrap()
        .execute_batch(
            "WITH RECURSIVE ids(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM ids WHERE n<1000)
             INSERT INTO job_runs(run_id, workspace_id, job_id, attempt, state, scheduled_at, created_at)
             SELECT 'other-' || n, 'ws_recency', 'other-job', 1,
                    CASE n%3 WHEN 0 THEN 'success' WHEN 1 THEN 'failed' ELSE 'interrupted' END,
                    '2026-11-01T00:00:00+00:00', '2026-11-01T00:00:00+00:00' FROM ids;",
        )
        .unwrap();
    let instructions = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let observed = instructions.clone();
    connection
        .lock()
        .unwrap()
        .trace_v2(
            TraceEventCodes::SQLITE_TRACE_PROFILE,
            Some(move |event: TraceEvent<'_>| {
                if let TraceEvent::Profile(statement, _) = event {
                    observed.fetch_add(
                        statement.get_status(StatementStatus::VmStep) as u64,
                        std::sync::atomic::Ordering::Relaxed,
                    );
                }
            }),
        )
        .unwrap();
    let runs = jobs
        .list_job_runs_filtered(&JobRunQuery {
            job_id: Some("job-without-runs".into()),
            states: vec![
                JobRunState::Success,
                JobRunState::Failed,
                JobRunState::Interrupted,
            ],
            order_by: JobRunOrder::Recency,
            limit: Some(100),
            include_steps: false,
            ..Default::default()
        })
        .unwrap();
    assert!(runs.is_empty());
    let instructions = instructions.load(std::sync::atomic::Ordering::Relaxed);
    assert!(instructions > 0, "the public store read must be observed");
    assert!(
        instructions < 1000,
        "per-job streak reads must seek the job prefix rather than scan unrelated history (ORB-22917): {instructions} SQLite instructions"
    );
}
