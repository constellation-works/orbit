//! Path leases are reservation rows held by a principal: a hard lease blocks
//! like a task reservation, an advisory one blocks nothing, and rows written
//! before leases read back as exclusive task reservations.

use super::*;
use orbit_store::contracts::{
    LeaseInsertParams, LeaseListResult, LeaseMode, LeasePrincipalClass, LeasePriority, LeaseRecord,
    LeaseReleaseParams, LeaseRenewParams, TaskLockConflict, TaskLockHolder,
    TaskReservationCheckParams, TaskReservationKind, TaskReservationReleaseReason,
};

fn lease_params(owner: &Coordinated, selector: &str, mode: LeaseMode) -> LeaseInsertParams {
    LeaseInsertParams {
        workspace_orbit_dir: owner.orbit_dir.to_string_lossy().into_owned(),
        workspace_id: Some(PARTITION_ID.into()),
        selectors: vec![selector.into()],
        mode,
        priority: LeasePriority::Normal,
        tags: vec!["refactor".into()],
        principal: "alice/claude-code".into(),
        principal_class: LeasePrincipalClass::Agent,
        machine_id: Some("hm_alice".into()),
        base_branch: Some("agent-main".into()),
        bound_branch: None,
        actor: "alice".into(),
        ttl_seconds: 600,
        token_id: None,
        wait: false,
        task_conflicts: Vec::new(),
        aging_minutes: 30,
        active_lease_limit: None,
    }
}

fn grant(owner: &Coordinated, params: LeaseInsertParams) -> LeaseRecord {
    let inserted = owner.backends.reservation.insert_lease(params).unwrap();
    assert!(inserted.conflicts.is_empty(), "{:?}", inserted.conflicts);
    inserted.lease.expect("lease recorded")
}

fn active_leases(owner: &Coordinated) -> LeaseListResult {
    owner
        .backends
        .reservation
        .list_active_leases(&owner.orbit_dir.to_string_lossy(), Some(PARTITION_ID))
        .unwrap()
}

#[test]
fn lease_store_renews_releases_and_expires_and_lists_only_active_leases() {
    if !allocation_admission_isolated(
        "path_leases::lease_store_renews_releases_and_expires_and_lists_only_active_leases",
    ) {
        return;
    }
    let owner = Coordinated::fixture();
    let orbit_dir = owner.orbit_dir.to_string_lossy().into_owned();
    let reservations = &owner.backends.reservation;

    let hard = grant(&owner, lease_params(&owner, "dir:src", LeaseMode::Hard));
    assert!(hard.lease_id.starts_with("lease-"), "{}", hard.lease_id);
    assert_eq!(hard.mode, LeaseMode::Hard);
    assert_eq!(hard.principal, "alice/claude-code");
    assert_eq!(hard.principal_class, LeasePrincipalClass::Agent);
    assert_eq!(hard.priority, LeasePriority::Normal);
    assert_eq!(hard.tags, ["refactor"]);
    assert_eq!(hard.machine_id.as_deref(), Some("hm_alice"));
    assert_eq!(hard.base_branch.as_deref(), Some("agent-main"));
    assert_eq!(hard.selectors, ["dir:src"]);
    assert_eq!(hard.heartbeat_at, hard.created_at);
    assert!(hard.released_at.is_none());
    assert_eq!(
        reservations
            .get_lease(&orbit_dir, Some(PARTITION_ID), &hard.lease_id)
            .unwrap()
            .as_ref(),
        Some(&hard)
    );

    // A second hard lease over a held path is refused and names the holder;
    // an advisory one over the same path is recorded.
    let refused = reservations
        .insert_lease(lease_params(&owner, "file:src/lib.rs", LeaseMode::Hard))
        .unwrap();
    assert!(refused.lease.is_none());
    assert_eq!(
        refused.conflicts,
        [TaskLockConflict {
            file: "file:src/lib.rs".into(),
            held_by: TaskLockHolder::Lease,
            held_by_id: hard.lease_id.clone(),
        }]
    );
    let advisory = grant(
        &owner,
        lease_params(&owner, "file:src/lib.rs", LeaseMode::Advisory),
    );
    assert_eq!(advisory.mode, LeaseMode::Advisory);

    // Renewal moves `expires_at` forward and stamps the heartbeat.
    let renewed = reservations
        .renew_lease(LeaseRenewParams {
            workspace_orbit_dir: orbit_dir.clone(),
            workspace_id: Some(PARTITION_ID.into()),
            lease_id: hard.lease_id.clone(),
            ttl_seconds: 3_600,
        })
        .unwrap()
        .lease
        .expect("active lease renews");
    let expiry = |lease: &LeaseRecord| DateTime::parse_from_rfc3339(&lease.expires_at).unwrap();
    assert!(
        expiry(&renewed) > expiry(&hard),
        "{} must move past {}",
        renewed.expires_at,
        hard.expires_at
    );
    assert!(
        DateTime::parse_from_rfc3339(&renewed.heartbeat_at).unwrap()
            >= DateTime::parse_from_rfc3339(&hard.heartbeat_at).unwrap()
    );

    // Release ends the advisory lease; it is not revived by a renewal.
    let released = reservations
        .release_lease(LeaseReleaseParams {
            workspace_orbit_dir: orbit_dir.clone(),
            workspace_id: Some(PARTITION_ID.into()),
            lease_id: advisory.lease_id.clone(),
            release_reason: TaskReservationReleaseReason::Explicit,
            release_metadata_json: None,
        })
        .unwrap()
        .lease
        .expect("active lease releases");
    assert!(released.released_at.is_some());
    assert_eq!(
        released.release_reason,
        Some(TaskReservationReleaseReason::Explicit)
    );
    let renew_released = reservations
        .renew_lease(LeaseRenewParams {
            workspace_orbit_dir: orbit_dir.clone(),
            workspace_id: Some(PARTITION_ID.into()),
            lease_id: advisory.lease_id.clone(),
            ttl_seconds: 600,
        })
        .unwrap();
    assert!(renew_released.lease.is_none());

    // A lease whose TTL has passed is expired by the next lease read.
    let lapsed = grant(
        &owner,
        LeaseInsertParams {
            ttl_seconds: 0,
            ..lease_params(&owner, "file:docs/notes.md", LeaseMode::Advisory)
        },
    );
    let listed = active_leases(&owner);
    assert_eq!(
        listed
            .leases
            .iter()
            .map(|lease| lease.lease_id.as_str())
            .collect::<Vec<_>>(),
        [hard.lease_id.as_str()],
        "list-active excludes released and expired leases"
    );
    assert_eq!(
        listed
            .expired_leases
            .iter()
            .map(|expired| expired.reservation_id.as_str())
            .collect::<Vec<_>>(),
        [lapsed.lease_id.as_str()]
    );
    let expired = reservations
        .get_lease(&orbit_dir, Some(PARTITION_ID), &lapsed.lease_id)
        .unwrap()
        .expect("expired lease stays readable");
    assert_eq!(
        expired.release_reason,
        Some(TaskReservationReleaseReason::TtlExpired)
    );

    // Lease rows are not task reservations: a task-reservation release by id
    // leaves the lease alone.
    let task_release = reservations
        .release_task_reservation(orbit_store::contracts::TaskReservationReleaseParams {
            workspace_orbit_dir: orbit_dir.clone(),
            workspace_id: Some(PARTITION_ID.into()),
            reservation_id: hard.lease_id.clone(),
            release_reason: TaskReservationReleaseReason::Explicit,
            release_metadata_json: None,
        })
        .unwrap();
    assert!(!task_release.released);
    assert_eq!(active_leases(&owner).leases.len(), 1);
}

#[test]
fn owner_pull_defers_on_a_hard_lease_and_admits_past_an_advisory_one() {
    if !allocation_admission_isolated(
        "path_leases::owner_pull_defers_on_a_hard_lease_and_admits_past_an_advisory_one",
    ) {
        return;
    }
    let owner = Coordinated::fixture();
    let task = owner.create_task_in("leased path", &["file:src/lib.rs"]);

    let hard = grant(&owner, lease_params(&owner, "dir:src", LeaseMode::Hard));
    let deferred = owner.pull(&owner_request("hard"));
    assert!(deferred.claim.is_none(), "a hard lease holds the footprint");
    assert_eq!(deferred.deferred_conflicts.len(), 1);
    assert_eq!(deferred.deferred_conflicts[0].task_id, task.id);
    assert_eq!(
        deferred.deferred_conflicts[0].blocked_by,
        std::slice::from_ref(&hard.lease_id)
    );
    assert!(
        deferred.deferred_conflicts[0]
            .reason
            .contains(&hard.lease_id),
        "{}",
        deferred.deferred_conflicts[0].reason
    );
    assert_eq!(owner.task_status(&task.id), TaskStatus::Backlog);

    owner
        .backends
        .reservation
        .release_lease(LeaseReleaseParams {
            workspace_orbit_dir: owner.orbit_dir.to_string_lossy().into_owned(),
            workspace_id: Some(PARTITION_ID.into()),
            lease_id: hard.lease_id,
            release_reason: TaskReservationReleaseReason::Explicit,
            release_metadata_json: None,
        })
        .unwrap();
    grant(&owner, lease_params(&owner, "dir:src", LeaseMode::Advisory));
    let admitted = owner.pull(&owner_request("advisory"));
    assert_eq!(
        admitted
            .claim
            .expect("an advisory lease holds nothing")
            .task_id,
        task.id
    );
    assert!(admitted.deferred_conflicts.is_empty());
}

#[test]
fn pre_lease_reservations_read_back_as_exclusive_task_reservations() {
    if !allocation_admission_isolated(
        "path_leases::pre_lease_reservations_read_back_as_exclusive_task_reservations",
    ) {
        return;
    }
    let root = TempDir::new().unwrap();
    let database = root.path().join("state.sqlite");
    drop(Store::open(&database).unwrap());
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute_batch(
            "DROP INDEX idx_task_reservations_kind_release;
             ALTER TABLE task_reservations DROP COLUMN kind;
             ALTER TABLE task_reservations DROP COLUMN mode;
             ALTER TABLE task_reservations DROP COLUMN principal;
             ALTER TABLE task_reservations DROP COLUMN principal_class;
             ALTER TABLE task_reservations DROP COLUMN machine_id;
             ALTER TABLE task_reservations DROP COLUMN priority;
             ALTER TABLE task_reservations DROP COLUMN tags_json;
             ALTER TABLE task_reservations DROP COLUMN base_branch;
             ALTER TABLE task_reservations DROP COLUMN bound_branch;
             ALTER TABLE task_reservations DROP COLUMN heartbeat_at;
             DELETE FROM schema_meta WHERE key LIKE 'migration.v%' AND key >= 'migration.v0061';",
        )
        .expect("restore the pre-lease reservation shape");
    connection
        .execute(
            "INSERT INTO task_reservations(reservation_id, workspace_orbit_dir, workspace_id,
                task_ids_json, files_json, actor, created_at, expires_at, scope)
             VALUES ('reservation-legacy', 'fixture', NULL, '[\"T-1\"]', '[\"file:shared.txt\"]',
                'operator', ?1, ?2, 'files')",
            rusqlite::params![
                Utc::now().to_rfc3339(),
                (Utc::now() + chrono::Duration::minutes(10)).to_rfc3339()
            ],
        )
        .unwrap();
    drop(connection);

    let store = Store::open(&database).expect("the lease migration applies");
    let active = store
        .inspect_active_task_reservations("fixture", None)
        .unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].reservation_id, "reservation-legacy");
    assert_eq!(active[0].kind, TaskReservationKind::TaskReservation);
    let check = store
        .check_task_reservation_conflicts(&TaskReservationCheckParams {
            workspace_orbit_dir: "fixture".into(),
            workspace_id: None,
            requested_files: vec!["file:shared.txt".into()],
        })
        .unwrap();
    assert_eq!(
        check.conflicts,
        [TaskLockConflict {
            file: "file:shared.txt".into(),
            held_by: TaskLockHolder::Reservation,
            held_by_id: "reservation-legacy".into(),
        }],
        "a pre-lease reservation stays exclusive"
    );
}
