//! The conflict footprint is checked in the insertion transaction even when
//! the successful grant would hold no files.

use super::*;
use orbit_store::contracts::TaskReservationReserveParams;

#[test]
fn empty_file_grant_waits_for_a_competing_reservation_transaction() {
    if !isolated(
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
            owner_metadata_json: None,
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
