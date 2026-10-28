use orbit_store::contracts::{
    CPU_ACCOUNTING_LINUX, ResourceHistoryKeep, ResourceMinutesTable, ResourceUsageStoreBackend,
    RunResourceMinute,
};

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
        "retention prunes older samples before the minute-zero freshness marker"
    );
    assert_eq!(
        store
            .delivery_runs_freshness()
            .expect("marker is retained")
            .finished_runs,
        1,
        "the marker stays while later run history is still being pruned"
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
            .expect("prune final marker"),
        1,
        "retention removes the minute-zero marker after the run's other history"
    );
    assert_eq!(
        store
            .delivery_runs_freshness()
            .expect("freshness after retention"),
        Default::default(),
        "removing sampled history invalidates the finished-run key"
    );
}
