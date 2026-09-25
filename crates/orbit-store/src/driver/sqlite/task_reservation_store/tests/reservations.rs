// Migrated from sqlite/task_reservation_store.rs per ORB-00231
use super::super::*;
use crate::Store;
use crate::TaskReservationCheckParams;
use crate::TaskReservationOwnedConflictsParams;
use crate::TaskReservationReleaseByOwnerParams;
use crate::TaskReservationReleaseParams;
use crate::TaskReservationReleaseReason;
use crate::TaskReservationReserveParams;
use rusqlite::params;

fn reserve_params(file: &str) -> TaskReservationReserveParams {
    TaskReservationReserveParams {
        workspace_orbit_dir: "/workspace/.orbit".to_string(),
        workspace_id: None,
        task_ids: vec!["T1".to_string()],
        requested_files: vec![file.to_string()],
        actor: "test".to_string(),
        ttl_seconds: 3600,
        owner_run_id: None,
        owner_metadata_json: None,
    }
}

#[test]
fn task_reservation_workspace_id_scopes_rows_and_still_sees_legacy_path_rows() {
    let store = Store::open_in_memory().expect("open store");

    let mut legacy = reserve_params("file:src/legacy.rs");
    legacy.task_ids = vec!["T-legacy".to_string()];
    let legacy_result = store
        .reserve_task_reservation(&legacy)
        .expect("reserve legacy path row");
    assert!(legacy_result.reserved);

    let mut scoped = reserve_params("file:src/scoped.rs");
    scoped.workspace_id = Some("repo-abcdef".to_string());
    scoped.task_ids = vec!["ORB-00001".to_string()];
    let scoped_result = store
        .reserve_task_reservation(&scoped)
        .expect("reserve scoped row");
    assert!(scoped_result.reserved);

    let mut other = reserve_params("file:src/other.rs");
    other.workspace_id = Some("other-abcdef".to_string());
    let other_result = store
        .reserve_task_reservation(&other)
        .expect("reserve other workspace");
    assert!(other_result.reserved);

    let active = store
        .list_active_task_reservations("/workspace/.orbit", Some("repo-abcdef"))
        .expect("list scoped active reservations");
    let ids = active
        .reservations
        .iter()
        .map(|reservation| reservation.task_ids.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        ids,
        vec![vec!["T-legacy".to_string()], vec!["ORB-00001".to_string()]]
    );
    assert_eq!(active.reservations[0].workspace_id, None);
    assert_eq!(
        active.reservations[1].workspace_id.as_deref(),
        Some("repo-abcdef")
    );

    let conflicts = store
        .check_task_reservation_conflicts(&TaskReservationCheckParams {
            workspace_orbit_dir: "/workspace/.orbit".to_string(),
            workspace_id: Some("other-abcdef".to_string()),
            requested_files: vec!["file:src/scoped.rs".to_string()],
        })
        .expect("check other scope");
    assert!(conflicts.conflicts.is_empty());
}

#[test]
fn task_reservation_persists_nullable_owner_context() {
    let store = Store::open_in_memory().expect("open store");

    let mut unowned = reserve_params("file:src/lib.rs");
    unowned.task_ids = vec!["T-unowned".to_string()];
    let unowned_result = store
        .reserve_task_reservation(&unowned)
        .expect("reserve unowned");
    assert!(unowned_result.reserved);

    let mut owned = reserve_params("file:src/main.rs");
    owned.task_ids = vec!["T-owned".to_string()];
    owned.owner_run_id = Some("jrun-owner".to_string());
    owned.owner_metadata_json = Some(r#"{"source":"test"}"#.to_string());
    let owned_result = store
        .reserve_task_reservation(&owned)
        .expect("reserve owned");
    assert!(owned_result.reserved);

    let active = store
        .list_active_task_reservations("/workspace/.orbit", None)
        .expect("list active");
    assert_eq!(active.reservations.len(), 2);
    let unowned_active = active
        .reservations
        .iter()
        .find(|reservation| {
            Some(reservation.reservation_id.as_str()) == unowned_result.reservation_id.as_deref()
        })
        .expect("unowned reservation");
    assert_eq!(unowned_active.owner_run_id, None);
    assert_eq!(unowned_active.owner_metadata_json, None);
    let owned_active = active
        .reservations
        .iter()
        .find(|reservation| {
            Some(reservation.reservation_id.as_str()) == owned_result.reservation_id.as_deref()
        })
        .expect("owned reservation");
    assert_eq!(owned_active.owner_run_id.as_deref(), Some("jrun-owner"));
    assert_eq!(
        owned_active.owner_metadata_json.as_deref(),
        Some(r#"{"source":"test"}"#)
    );
}

#[test]
fn task_reservation_owner_batch_release_preserves_unowned_rows() {
    let store = Store::open_in_memory().expect("open store");
    let unowned = store
        .reserve_task_reservation(&reserve_params("file:src/lib.rs"))
        .expect("reserve unowned");

    let mut owned = reserve_params("file:src/main.rs");
    owned.owner_run_id = Some("jrun-owner".to_string());
    let owned_result = store
        .reserve_task_reservation(&owned)
        .expect("reserve owned");

    let released = store
        .release_task_reservations_by_owner_run_id(&TaskReservationReleaseByOwnerParams {
            workspace_orbit_dir: "/workspace/.orbit".to_string(),
            workspace_id: None,
            owner_run_id: "jrun-owner".to_string(),
            release_reason: TaskReservationReleaseReason::RunTerminal,
            release_metadata_json: Some(r#"{"why":"terminal"}"#.to_string()),
        })
        .expect("release owner");

    assert_eq!(released.released_reservations.len(), 1);
    assert_eq!(
        released.released_reservations[0].reservation_id,
        owned_result.reservation_id.expect("owned reservation id")
    );
    assert_eq!(
        released.released_reservations[0].release_reason,
        TaskReservationReleaseReason::RunTerminal
    );

    let active = store
        .list_active_task_reservations("/workspace/.orbit", None)
        .expect("list active");
    assert_eq!(active.reservations.len(), 1);
    assert_eq!(
        Some(active.reservations[0].reservation_id.as_str()),
        unowned.reservation_id.as_deref()
    );
    assert_eq!(active.reservations[0].owner_run_id, None);
}

#[test]
fn task_reservation_explicit_release_is_idempotent_without_metadata_churn() {
    let store = Store::open_in_memory().expect("open store");
    let mut params = reserve_params("file:src/lib.rs");
    params.owner_run_id = Some("jrun-owner".to_string());
    let reservation = store
        .reserve_task_reservation(&params)
        .expect("reserve")
        .reservation_id
        .expect("reservation id");

    let first = store
        .release_task_reservation(&TaskReservationReleaseParams {
            workspace_orbit_dir: "/workspace/.orbit".to_string(),
            workspace_id: None,
            reservation_id: reservation.clone(),
            release_reason: TaskReservationReleaseReason::Explicit,
            release_metadata_json: Some(r#"{"first":true}"#.to_string()),
        })
        .expect("release first");
    assert!(first.released);
    assert_eq!(
        first
            .reservation
            .as_ref()
            .and_then(|reservation| reservation.owner_run_id.as_deref()),
        Some("jrun-owner")
    );

    let second = store
        .release_task_reservation(&TaskReservationReleaseParams {
            workspace_orbit_dir: "/workspace/.orbit".to_string(),
            workspace_id: None,
            reservation_id: reservation.clone(),
            release_reason: TaskReservationReleaseReason::RunTerminal,
            release_metadata_json: Some(r#"{"second":true}"#.to_string()),
        })
        .expect("release second");
    assert!(!second.released);

    let conn = store.connection();
    let guard = conn.lock().expect("conn lock");
    let (reason, metadata): (String, Option<String>) = guard
        .query_row(
            "SELECT release_reason, release_metadata_json
                 FROM task_reservations
                 WHERE reservation_id = ?1",
            params![reservation],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("query release metadata");
    assert_eq!(reason, "explicit");
    assert_eq!(metadata.as_deref(), Some(r#"{"first":true}"#));
}

#[test]
fn task_reservation_owned_conflicts_return_owner_fields() {
    let store = Store::open_in_memory().expect("open store");
    let mut params = reserve_params("file:src/lib.rs");
    params.owner_run_id = Some("jrun-owner".to_string());
    params.owner_metadata_json = Some(r#"{"source":"test"}"#.to_string());
    store
        .reserve_task_reservation(&params)
        .expect("reserve owned");

    let conflicts = store
        .list_owned_task_reservation_conflicts(&TaskReservationOwnedConflictsParams {
            workspace_orbit_dir: "/workspace/.orbit".to_string(),
            workspace_id: None,
            requested_files: vec!["file:src/lib.rs".to_string()],
            limit: 10,
        })
        .expect("owned conflicts");

    assert_eq!(conflicts.reservations.len(), 1);
    assert_eq!(
        conflicts.reservations[0].owner_run_id.as_deref(),
        Some("jrun-owner")
    );
    assert_eq!(
        conflicts.reservations[0].owner_metadata_json.as_deref(),
        Some(r#"{"source":"test"}"#)
    );
}

#[test]
fn task_reservation_owned_conflict_limit_applies_after_overlap_filter() {
    let store = Store::open_in_memory().expect("open store");
    for index in 0..3 {
        let mut params = reserve_params(&format!("file:src/non_overlap_{index}.rs"));
        params.owner_run_id = Some(format!("jrun-non-overlap-{index}"));
        store.reserve_task_reservation(&params).expect("reserve");
    }

    let mut overlapping = reserve_params("file:src/target.rs");
    overlapping.owner_run_id = Some("jrun-target".to_string());
    store
        .reserve_task_reservation(&overlapping)
        .expect("reserve overlapping");

    let conflicts = store
        .list_owned_task_reservation_conflicts(&TaskReservationOwnedConflictsParams {
            workspace_orbit_dir: "/workspace/.orbit".to_string(),
            workspace_id: None,
            requested_files: vec!["file:src/target.rs".to_string()],
            limit: 1,
        })
        .expect("owned conflicts");

    assert_eq!(conflicts.reservations.len(), 1);
    assert_eq!(
        conflicts.reservations[0].owner_run_id.as_deref(),
        Some("jrun-target")
    );
}

/// Two reservations minted back to back must never share a primary key,
/// even when the clock does not advance between them.
#[test]
fn reservation_ids_are_unique_within_a_clock_tick() {
    let ids: std::collections::BTreeSet<String> =
        (0..1_000).map(|_| unique_row_id("reservation-")).collect();
    assert_eq!(ids.len(), 1_000);
    assert!(ids.iter().all(|id| id.starts_with("reservation-")));

    let store = Store::open_in_memory().expect("open store");
    let first = store
        .reserve_task_reservation(&reserve_params("file:src/a.rs"))
        .expect("reserve a");
    let mut second_params = reserve_params("file:src/b.rs");
    second_params.task_ids = vec!["T2".to_string()];
    let second = store
        .reserve_task_reservation(&second_params)
        .expect("reserve b");
    assert!(first.reserved && second.reserved);
    assert_ne!(first.reservation_id, second.reservation_id);
}
