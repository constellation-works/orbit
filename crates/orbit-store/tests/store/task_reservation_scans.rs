// Retained reservation history must not turn active reads into table scans.
#[test]
fn retained_task_reservation_reads_do_not_scan_the_table() {
    use orbit_store::contracts::{
        TaskReservationCheckParams, TaskReservationReleaseByOwnerParams,
        TaskReservationReleaseParams, TaskReservationReleaseReason,
    };
    use rusqlite::StatementStatus;
    use rusqlite::trace::{TraceEvent, TraceEventCodes};
    use std::sync::{Arc, Mutex};

    let store = Store::open_in_memory().expect("open store");
    let connection = store.connection();
    connection
        .lock()
        .expect("store connection")
        .execute_batch(
            "WITH RECURSIVE ids(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM ids WHERE n<400)
             INSERT INTO task_reservations(
                 reservation_id, workspace_orbit_dir, workspace_id, task_ids_json, files_json,
                 actor, created_at, expires_at, released_at
             )
             SELECT 'released-' || n, '/ws', 'ws', '[\"t\"]', '[\"old.rs\"]', 'actor',
                    '2020-01-01T00:00:00Z', '2020-01-02T00:00:00Z', '2020-01-03T00:00:00Z'
             FROM ids;
             INSERT INTO task_reservations(
                 reservation_id, workspace_orbit_dir, workspace_id, task_ids_json, files_json,
                 actor, created_at, expires_at
             ) VALUES
                 ('active-legacy', '/ws', NULL, '[\"t\"]', '[\"legacy.rs\"]', 'actor',
                  '2026-01-01T00:00:01Z', '2099-01-01T00:00:00Z'),
                 ('active-ws', '/ws', 'ws', '[\"t\"]', '[\"held.rs\"]', 'actor',
                  '2026-01-01T00:00:02Z', '2099-01-01T00:00:00Z'),
                 ('active-other', '/ws', 'other', '[\"t\"]', '[\"other.rs\"]', 'actor',
                  '2026-01-01T00:00:03Z', '2099-01-01T00:00:00Z'),
                 ('expired-ws', '/ws', 'ws', '[\"t\"]', '[\"expired.rs\"]', 'actor',
                  '2026-01-01T00:00:04Z', '2000-01-01T00:00:00Z');",
        )
        .expect("seed reservation history");

    let hits = Arc::new(Mutex::new(Vec::new()));
    let observed = hits.clone();
    connection
        .lock()
        .expect("store connection")
        .trace_v2(
            TraceEventCodes::SQLITE_TRACE_PROFILE,
            Some(move |event: TraceEvent<'_>| {
                if let TraceEvent::Profile(statement, _) = event {
                    let sql = statement.sql();
                    if sql.contains("task_reservations") && sql.contains("released_at IS NULL") {
                        observed.lock().expect("scan hits").push((
                            sql.to_string(),
                            statement.get_status(StatementStatus::FullscanStep),
                        ));
                    }
                }
            }),
        )
        .expect("trace reservation statements");

    let bound = store
        .inspect_active_task_reservations("/ws", Some("ws"))
        .expect("inspect bound workspace");
    assert_eq!(
        bound
            .iter()
            .map(|row| row.reservation_id.as_str())
            .collect::<Vec<_>>(),
        ["active-legacy", "active-ws"],
        "a workspace id sees its rows plus legacy null-workspace rows, and not released or expired history"
    );
    let legacy = store
        .inspect_active_task_reservations("/ws", None)
        .expect("inspect legacy workspace");
    assert_eq!(
        legacy
            .iter()
            .map(|row| row.reservation_id.as_str())
            .collect::<Vec<_>>(),
        ["active-legacy", "active-ws", "active-other"],
        "an absent workspace id sees every active row of the orbit dir"
    );

    let conflicts = store
        .check_task_reservation_conflicts(&TaskReservationCheckParams {
            workspace_orbit_dir: "/ws".to_string(),
            workspace_id: Some("ws".to_string()),
            requested_files: vec!["held.rs".to_string()],
        })
        .expect("check conflicts");
    assert_eq!(conflicts.conflicts.len(), 1);
    assert_eq!(conflicts.conflicts[0].held_by_id, "active-ws");
    assert!(
        conflicts
            .expired_reservations
            .iter()
            .any(|row| row.reservation_id == "expired-ws")
    );
    store
        .check_task_reservation_conflicts(&TaskReservationCheckParams {
            workspace_orbit_dir: "/ws".to_string(),
            workspace_id: None,
            requested_files: vec!["other.rs".to_string()],
        })
        .expect("check legacy conflicts");

    let history = store
        .list_task_reservation_history("/ws", Some("ws"))
        .expect("history");
    assert!(
        history.len() > 400,
        "history stays unbounded and still returns released rows"
    );
    assert!(history.iter().any(|row| row.reservation_id == "released-1"));
    assert!(
        history
            .iter()
            .all(|row| row.reservation_id != "active-other")
    );

    store
        .release_task_reservation(&TaskReservationReleaseParams {
            workspace_orbit_dir: "/ws".to_string(),
            workspace_id: Some("ws".to_string()),
            reservation_id: "active-ws".to_string(),
            release_reason: TaskReservationReleaseReason::Explicit,
            release_metadata_json: None,
        })
        .expect("release");
    store
        .release_task_reservations_by_owner_run_id(&TaskReservationReleaseByOwnerParams {
            workspace_orbit_dir: "/ws".to_string(),
            workspace_id: None,
            owner_run_id: "run-absent".to_string(),
            release_reason: TaskReservationReleaseReason::Explicit,
            release_metadata_json: None,
        })
        .expect("release by owner");
    store
        .inspect_active_leases("/ws", Some("ws"))
        .expect("inspect leases");
    store.list_active_leases("/ws", None).expect("list leases");
    store
        .show_workspace_claim("/ws", Some("ws"))
        .expect("show claim");

    let hits = hits.lock().expect("scan hits");
    assert!(
        hits.iter().any(|(sql, _)| sql.contains("expires_at >")),
        "the trace must observe an active-row read"
    );
    assert!(
        hits.iter().any(|(sql, _)| sql.contains("expires_at <=")),
        "the trace must observe the expiry sweep"
    );
    let scanned: Vec<_> = hits
        .iter()
        .filter(|(_, steps)| *steps != 0)
        .map(|(sql, steps)| format!("{steps} fullscan steps: {sql}"))
        .collect();
    assert!(
        scanned.is_empty(),
        "active task_reservations statements must not scan retained history (ORB-24410): {scanned:?}"
    );
}
