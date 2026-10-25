// Legacy-schema setup freezes the persisted input; the regression exercises the
// public store API across an upgrade, updates and deletion.
#[test]
fn historical_task_deliveries_remain_visible_after_upgrade_and_rebinding() {
    if !migration_freeze_isolated(
        "historical_task_deliveries_remain_visible_after_upgrade_and_rebinding",
    ) {
        return;
    }
    let dir = tempfile::tempdir().expect("fixture");
    let path = dir.path().join("orbit.db");
    drop(Store::open(&path).expect("fixture schema"));
    let conn = Connection::open(&path).expect("database");
    conn.execute_batch(
        "DROP TRIGGER job_run_tasks_insert;
        DROP TRIGGER job_run_tasks_update; DROP TRIGGER job_run_tasks_delete;
        DROP TABLE job_run_tasks;
        DELETE FROM schema_meta WHERE key='migration.v0059';",
    )
    .expect("restore pre-ownership schema");
    let ids = (0..101).map(|n| format!("T-{n:03}")).collect::<Vec<_>>();
    for (index, id) in ids.iter().enumerate() {
        conn.execute(
            "INSERT INTO job_runs(workspace_id, run_id, job_id, attempt, state,
             scheduled_at, created_at, input_json) VALUES (?1, ?2, 'task_pr_pipeline',
             1, 'failed', '2026-10-01T00:00:00+00:00', '2026-10-01T00:00:00+00:00', ?3)",
            rusqlite::params![
                "ws_a",
                format!("jrun-old-{index:03}"),
                json!({"task_ids": [id, id, 3, null], "task_id": id}).to_string()
            ],
        )
        .expect("historical failed delivery");
    }
    drop(conn);
    let store = Store::open(&path).expect("upgrade backfills historical ownership");
    let jobs = workspace_job_run_store(store.clone(), "ws_a");
    let mut requested = ids.clone();
    requested.extend(ids.clone());
    let found = jobs
        .recent_job_runs_for_tasks("task_pr_pipeline", &requested, 8)
        .expect("bounded batches");
    assert_eq!(
        found.len(),
        101,
        "every task survives the batched history lookup"
    );
    assert!(
        found.values().all(|runs| runs.len() == 1),
        "duplicate bindings do not duplicate evidence"
    );
    assert!(
        workspace_job_run_store(store.clone(), "ws_b")
            .recent_job_runs_for_tasks("task_pr_pipeline", &ids, 8)
            .expect("other workspace")
            .is_empty()
    );
    let mut run = found[&ids[0]][0].clone();
    run.input = Some(json!({"task_id": "T-rebound", "nested": {"task_id": ids[0]}}));
    run.finished_at = Some(chrono::Utc::now());
    store
        .upsert_job_run_for_workspace("ws_a", &run, None)
        .expect("rebind");
    assert!(
        jobs.recent_job_runs_for_tasks("task_pr_pipeline", std::slice::from_ref(&ids[0]), 8)
            .expect("old binding")
            .is_empty()
    );
    let rebound = ["T-rebound".to_string()];
    assert_eq!(
        jobs.recent_job_runs_for_tasks("task_pr_pipeline", &rebound, 8)
            .expect("new binding")["T-rebound"][0]
            .finished_at,
        run.finished_at
    );
    jobs.delete_job_run(&run.run_id).expect("delete delivery");
    assert!(
        jobs.recent_job_runs_for_tasks("task_pr_pipeline", &rebound, 8)
            .expect("deleted binding")
            .is_empty()
    );
}
