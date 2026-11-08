//! The conflict footprint is checked in the insertion transaction even when
//! the successful grant would hold no files.

use super::*;
use orbit_store::contracts::TaskReservationReserveParams;
use orbit_types::task::{PipelineChildAdmissionMeta, ReservationOwnerMetadata};

#[test]
fn empty_file_grant_waits_for_a_competing_reservation_transaction() {
    if !allocation_admission_isolated(
        "reservation_grants::empty_file_grant_waits_for_a_competing_reservation_transaction",
    ) {
        return;
    }
    let root = TempDir::new().unwrap();
    let database = root.path().join("state.sqlite");
    let store = Store::open(&database).unwrap();
    let mut connection = rusqlite::Connection::open(&database).unwrap();
    let transaction = connection
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    // This competing grant is invisible to other connections until commit.
    transaction.execute(
        "INSERT INTO task_reservations(reservation_id, workspace_orbit_dir, workspace_id, task_ids_json,
            files_json, actor, created_at, expires_at, scope)
         VALUES ('reservation-competitor', 'fixture', NULL, '[]', '[\"file:shared.txt\"]',
            'operator', ?1, ?2, 'files')",
        rusqlite::params![Utc::now().to_rfc3339(), (Utc::now() + chrono::Duration::minutes(1)).to_rfc3339()],
    ).unwrap();
    let (started, arrival) = std::sync::mpsc::sync_channel(1);
    let (finished, completion) = std::sync::mpsc::sync_channel(1);
    let candidate = std::thread::spawn(move || {
        started.send(()).unwrap();
        let result = store.reserve_task_reservation(&TaskReservationReserveParams {
            workspace_orbit_dir: "fixture".into(),
            workspace_id: None,
            task_ids: Vec::new(),
            requested_files: vec!["file:shared.txt".into()],
            stored_files: Vec::new(),
            actor: "reviewer".into(),
            ttl_seconds: 120,
            owner_run_id: None,
            owner_metadata: None,
        });
        finished.send(result).unwrap();
    });
    arrival.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(
        matches!(
            completion.recv_timeout(Duration::from_millis(100)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ),
        "the grant must wait for the competing write transaction"
    );
    transaction.commit().unwrap();
    let refused = completion
        .recv_timeout(Duration::from_secs(10))
        .unwrap()
        .unwrap();
    candidate.join().unwrap();
    assert!(
        !refused.reserved,
        "the committed competing footprint must deny the empty-file grant"
    );
    assert_eq!(refused.conflicts.len(), 1);
    assert_eq!(refused.conflicts[0].held_by_id, "reservation-competitor");
    assert!(refused.reservation_id.is_none());
    let count: i64 = connection
        .query_row("SELECT count(*) FROM task_reservations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, 1, "the rejected grant must not insert a row");
}

#[test]
fn old_shape_owner_metadata_round_trips_at_the_store_boundary() {
    if !allocation_admission_isolated(
        "reservation_grants::old_shape_owner_metadata_round_trips_at_the_store_boundary",
    ) {
        return;
    }
    let root = TempDir::new().unwrap();
    let database = root.path().join("state.sqlite");
    let store = Store::open(&database).unwrap();
    // A row as an older binary wrote it: the metadata document as JSON
    // text, including a key the typed struct does not name.
    let old_shape = r#"{"claim_id":"claim-old","source":"v2_activity","fs_profile":"workspace-write","pipeline_child_admission":{"action":"ship","parent_step_id":"step-1","blocking":true},"future_key":{"nested":[1,2]}}"#;
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection.execute(
        "INSERT INTO task_reservations(reservation_id, workspace_orbit_dir, workspace_id, task_ids_json,
            files_json, actor, created_at, expires_at, owner_run_id, owner_metadata_json, scope)
         VALUES ('reservation-old', 'fixture', NULL, '[]', '[\"file:old.txt\"]',
            'operator', ?1, ?2, 'jrun-old', ?3, 'files')",
        rusqlite::params![Utc::now().to_rfc3339(), (Utc::now() + chrono::Duration::minutes(5)).to_rfc3339(), old_shape],
    ).unwrap();
    drop(connection);

    let reservations = store
        .inspect_active_task_reservations("fixture", None)
        .unwrap();
    assert_eq!(reservations.len(), 1);
    let metadata = reservations[0].owner_metadata.as_ref().unwrap();
    assert_eq!(metadata.claim_id.as_deref(), Some("claim-old"));
    assert_eq!(metadata.source.as_deref(), Some("v2_activity"));
    assert_eq!(metadata.fs_profile.as_deref(), Some("workspace-write"));
    assert_eq!(
        metadata.child_admission(),
        Some(&PipelineChildAdmissionMeta {
            action: "ship".to_string(),
            parent_step_id: Some("step-1".to_string()),
            blocking: true,
        })
    );
    assert_eq!(
        metadata.extra.get("future_key"),
        Some(&serde_json::json!({"nested": [1, 2]})),
        "unknown keys survive through the flattened remainder"
    );
    // Re-encoding produces the same document keys (order-insensitive),
    // and the record's serde shape still carries the document as a
    // string under `owner_metadata_json`.
    let encoded: serde_json::Value =
        serde_json::from_str(&metadata.encode_owner_metadata().unwrap()).unwrap();
    assert_eq!(
        encoded,
        serde_json::from_str::<serde_json::Value>(old_shape).unwrap()
    );
    let record = serde_json::to_value(&reservations[0]).unwrap();
    let wire_metadata = record["owner_metadata_json"].as_str().unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(wire_metadata).unwrap(),
        encoded
    );
    let decoded: ActiveTaskReservation = serde_json::from_value(record).unwrap();
    assert_eq!(decoded, reservations[0]);

    // A typed grant persists the same TEXT-column document shape.
    let granted = store
        .reserve_task_reservation(&TaskReservationReserveParams {
            workspace_orbit_dir: "fixture".into(),
            workspace_id: None,
            task_ids: Vec::new(),
            requested_files: vec!["file:new.txt".into()],
            stored_files: vec!["file:new.txt".into()],
            actor: "reviewer".into(),
            ttl_seconds: 120,
            owner_run_id: Some("jrun-new".into()),
            owner_metadata: Some(ReservationOwnerMetadata {
                claim_id: Some("claim-new".to_string()),
                extra: serde_json::Map::from_iter([(
                    "future_key".to_string(),
                    serde_json::json!("kept"),
                )]),
                ..Default::default()
            }),
        })
        .unwrap();
    assert!(granted.reserved);
    let connection = rusqlite::Connection::open(&database).unwrap();
    let stored: String = connection
        .query_row(
            "SELECT owner_metadata_json FROM task_reservations WHERE reservation_id = ?1",
            rusqlite::params![granted.reservation_id.as_deref().unwrap()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&stored).unwrap(),
        serde_json::json!({"claim_id": "claim-new", "future_key": "kept"})
    );
}
