use orbit_store::contracts::{ResourceHistoryKeep, ResourceMinutesTable};

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
        .read().unwrap().resource_usage().count_prunable(
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
