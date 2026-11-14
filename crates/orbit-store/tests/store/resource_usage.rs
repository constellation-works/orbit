use orbit_store::contracts::{
    CPU_ACCOUNTING_LINUX, ResourceHistoryKeep, ResourceMinutesTable, ResourceUsageStoreBackend,
    RunResourceMinute,
};

/// Retention must rank models in one pass, rather than scan a workspace's
/// entire minute history again for every eligible delivery (ORB-24645).
#[test]
fn resource_retention_selection_does_not_rescan_the_workspace_per_delivery() {
    const TEST: &str = "resource_retention_selection_does_not_rescan_the_workspace_per_delivery";
    let Some(_root) = store_retention_isolated(TEST, Duration::from_secs(60)) else {
        return;
    };
    use rusqlite::StatementStatus;
    use rusqlite::trace::{TraceEvent, TraceEventCodes};
    use std::sync::atomic::{AtomicU64, Ordering};

    let store = Store::open_in_memory().unwrap();
    let connection = store.connection();
    connection
        .lock()
        .unwrap()
        .execute_batch(
            "WITH RECURSIVE ids(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM ids WHERE n<600)
         INSERT INTO job_runs(workspace_id, run_id, job_id, attempt, state, scheduled_at,
                              created_at, finished_at)
         SELECT 'ws_retention', printf('run-%04d', n), 'task_auto_pipeline', 1, 'success',
                '2026-01-01', '2026-01-01', '2026-01-02' FROM ids;
         INSERT INTO run_resource_minutes(workspace_id, run_id, minute_index, minute_started_at,
             task_id, work_class, complexity, delivery_admitted_at, cpu_cores, memory_bytes,
             samples, cpu_accounting, crew_model)
         SELECT workspace_id, run_id, 0, '2026-01-01T00:00:00.000000Z', run_id, 'change',
                'medium', '2026-01-01T00:00:00.000000Z', 1, 1024, 1,
                'linux-cgroup-v1', 'fixture-model' FROM job_runs;",
        )
        .unwrap();
    let instructions = Arc::new(AtomicU64::new(0));
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
                        Ordering::Relaxed,
                    );
                }
            }),
        )
        .unwrap();
    let rows = store
        .resource_minutes_older_than(
            ResourceMinutesTable::Run,
            Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap(),
            ResourceHistoryKeep {
                deliveries_per_key: 1,
                forecasts: 1,
            },
        )
        .unwrap();
    assert_eq!(rows, 599);
    let steps = instructions.load(Ordering::Relaxed);
    assert!(steps > 0, "the public retention query was observed");
    assert!(
        steps < 1_200_000,
        "retention must not rescan the workspace per delivery (ORB-24645): {steps} SQLite instructions for 600 deliveries"
    );
}

#[test]
fn freshness_tracks_finished_runs_and_retention_removal() {
    let store = Store::open_in_memory().expect("memory store");
    let jobs = workspace_job_run_store(store.clone(), "ws_resource_usage");
    let admitted_at = Utc::now() - TimeDelta::days(3);
    let run = jobs
        .insert_job_run("resource_profile_fixture", 1, admitted_at, None, None)
        .expect("insert delivery");
    let minute_started_at = admitted_at + TimeDelta::minutes(2);
    store
        .record_run_resource_minute(&RunResourceMinute {
            workspace_id: "ws_resource_usage".into(),
            run_id: run.run_id.clone(),
            minute_index: 0,
            minute_started_at,
            task_id: "ORB-TEST".into(),
            work_class: "change".into(),
            complexity: Some("medium".into()),
            resumed: false,
            delivery_admitted_at: Some(admitted_at),
            cpu_cores: 0.5,
            cpu_accounting: Some(CPU_ACCOUNTING_LINUX.into()),
            memory_bytes: 1024,
            samples: 1,
            crew_model: None,
            orbit_version: None,
        })
        .expect("record first sample");
    store
        .record_run_resource_minute(&RunResourceMinute {
            workspace_id: "ws_resource_usage".into(),
            run_id: run.run_id.clone(),
            minute_index: 1,
            minute_started_at: minute_started_at + TimeDelta::minutes(1),
            task_id: "ORB-TEST".into(),
            work_class: "change".into(),
            complexity: Some("medium".into()),
            resumed: false,
            delivery_admitted_at: Some(admitted_at),
            cpu_cores: 0.5,
            cpu_accounting: Some(CPU_ACCOUNTING_LINUX.into()),
            memory_bytes: 1024,
            samples: 1,
            crew_model: None,
            orbit_version: None,
        })
        .expect("record following sample");

    assert_eq!(
        store.delivery_runs_freshness().expect("live freshness"),
        Default::default(),
        "an unfinished sampled run does not invalidate the finished-run key"
    );

    let finished_at = Utc::now();
    jobs.mark_job_run_running(&run.run_id, admitted_at, std::process::id())
        .expect("start delivery");
    jobs.finalize_job_run(&run.run_id, JobRunState::Success, finished_at, Some(1))
        .expect("finish delivery");
    let finished = store.delivery_runs_freshness().expect("finished freshness");
    assert_eq!(finished.finished_runs, 1);
    assert_eq!(
        finished
            .newest_finished_at
            .expect("finished timestamp")
            .timestamp_micros(),
        finished_at.timestamp_micros()
    );

    assert_eq!(
        store
            .prune_resource_minutes_batch(
                ResourceMinutesTable::Run,
                Utc::now(),
                ResourceHistoryKeep {
                    deliveries_per_key: 0,
                    forecasts: 0,
                },
                1,
            )
            .expect("prune retained history"),
        1,
        "retention prunes older samples before the minute-zero sample"
    );
    assert_eq!(
        store
            .delivery_runs_freshness()
            .expect("run is still counted")
            .finished_runs,
        1,
        "the run stays counted while its other history is still being pruned"
    );
    assert_eq!(
        store
            .prune_resource_minutes_batch(
                ResourceMinutesTable::Run,
                Utc::now(),
                ResourceHistoryKeep {
                    deliveries_per_key: 0,
                    forecasts: 0,
                },
                1,
            )
            .expect("prune final sample"),
        1,
        "retention removes the minute-zero sample after the run's other history"
    );
    assert_eq!(
        store
            .delivery_runs_freshness()
            .expect("freshness after retention"),
        Default::default(),
        "removing sampled history invalidates the finished-run key"
    );
}

#[test]
fn freshness_counts_a_finished_run_without_a_minute_zero_sample() {
    let store = Store::open_in_memory().expect("memory store");
    let jobs = workspace_job_run_store(store.clone(), "ws_resource_usage");
    let admitted_at = Utc::now() - TimeDelta::days(3);
    let run = jobs
        .insert_job_run("resource_profile_fixture", 1, admitted_at, None, None)
        .expect("insert delivery");
    // The run's first read failed or its minute-zero insert was lost, so the
    // first recorded sample is minute one.
    store
        .record_run_resource_minute(&RunResourceMinute {
            workspace_id: "ws_resource_usage".into(),
            run_id: run.run_id.clone(),
            minute_index: 1,
            minute_started_at: admitted_at + TimeDelta::minutes(1),
            task_id: "ORB-TEST".into(),
            work_class: "change".into(),
            complexity: Some("medium".into()),
            resumed: false,
            delivery_admitted_at: Some(admitted_at),
            cpu_cores: 0.5,
            cpu_accounting: Some(CPU_ACCOUNTING_LINUX.into()),
            memory_bytes: 1024,
            samples: 1,
            crew_model: None,
            orbit_version: None,
        })
        .expect("record only sample");

    assert_eq!(
        store.delivery_runs_freshness().expect("live freshness"),
        Default::default(),
        "an unfinished sampled run does not invalidate the finished-run key"
    );

    let finished_at = Utc::now();
    jobs.mark_job_run_running(&run.run_id, admitted_at, std::process::id())
        .expect("start delivery");
    jobs.finalize_job_run(&run.run_id, JobRunState::Success, finished_at, Some(1))
        .expect("finish delivery");
    let finished = store.delivery_runs_freshness().expect("finished freshness");
    assert_eq!(
        finished.finished_runs, 1,
        "a finished run with no minute-zero sample still changes the finished-run key"
    );
    assert_eq!(
        finished
            .newest_finished_at
            .expect("finished timestamp")
            .timestamp_micros(),
        finished_at.timestamp_micros()
    );
}

/// A profile rebuild reloads only runs whose minute rows or `job_runs` row
/// moved since its last load (ORB-21826); each reload must still equal a
/// whole read as the runs gain minutes, finish and lose history.
#[test]
fn reload_matches_a_full_read_as_runs_change() {
    let store = Store::open_in_memory().expect("memory store");
    let jobs = workspace_job_run_store(store.clone(), "ws_resource_usage");
    let admitted_at = Utc::now() - TimeDelta::days(3);
    let minute = |run_id: &str, minute_index: u32, crew_model: Option<&str>| RunResourceMinute {
        workspace_id: "ws_resource_usage".into(),
        run_id: run_id.into(),
        minute_index,
        minute_started_at: admitted_at + TimeDelta::minutes(i64::from(minute_index)),
        task_id: "ORB-TEST".into(),
        work_class: "change".into(),
        complexity: Some("medium".into()),
        resumed: false,
        delivery_admitted_at: Some(admitted_at),
        cpu_cores: 0.25 * f64::from(minute_index + 1),
        cpu_accounting: Some(CPU_ACCOUNTING_LINUX.into()),
        memory_bytes: 1024 * u64::from(minute_index + 1),
        samples: 6,
        crew_model: crew_model.map(str::to_string),
        orbit_version: Some("0.44.0".into()),
    };
    let live = jobs
        .insert_job_run("resource_profile_fixture", 1, admitted_at, None, None)
        .expect("insert live run")
        .run_id;
    let done = jobs
        .insert_job_run("resource_profile_fixture", 1, admitted_at, None, None)
        .expect("insert finished run")
        .run_id;
    for index in 0..2 {
        store
            .record_run_resource_minute(&minute(&live, index, Some("model-a")))
            .expect("record live minute");
        store
            .record_run_resource_minute(&minute(&done, index, None))
            .expect("record finished minute");
    }
    jobs.mark_job_run_running(&done, admitted_at, std::process::id())
        .expect("start finished run");
    jobs.finalize_job_run(&done, JobRunState::Success, Utc::now(), Some(1))
        .expect("finish run");
    let full = || {
        store
            .delivery_run_resources(DateTime::UNIX_EPOCH)
            .expect("full read")
    };

    let first = store.reload_delivery_runs(None).expect("first load");
    assert_eq!(first.runs, full(), "a first load is a full read");
    assert_eq!(first.runs.len(), 2);

    // The live run gains a minute, then finishes; the finished run is held.
    store
        .record_run_resource_minute(&minute(&live, 2, None))
        .expect("record another live minute");
    jobs.mark_job_run_running(&live, admitted_at, std::process::id())
        .expect("start live run");
    jobs.finalize_job_run(&live, JobRunState::Success, Utc::now(), Some(1))
        .expect("finish live run");
    let second = store
        .reload_delivery_runs(Some(first))
        .expect("reload after a finish");
    assert_eq!(
        second.runs,
        full(),
        "a run that gained a minute and finished is read again"
    );
    assert_eq!(
        second
            .runs
            .iter()
            .map(|run| run.minutes.len())
            .sum::<usize>(),
        5
    );

    // Retention removes one minute of history.
    assert_eq!(
        store
            .prune_resource_minutes_batch(
                ResourceMinutesTable::Run,
                Utc::now(),
                ResourceHistoryKeep {
                    deliveries_per_key: 0,
                    forecasts: 0,
                },
                1,
            )
            .expect("prune one minute"),
        1
    );
    let third = store
        .reload_delivery_runs(Some(second))
        .expect("reload after pruning");
    assert_eq!(third.runs, full(), "a run that lost a minute is read again");
    assert_eq!(
        third
            .runs
            .iter()
            .map(|run| run.minutes.len())
            .sum::<usize>(),
        4
    );
}
