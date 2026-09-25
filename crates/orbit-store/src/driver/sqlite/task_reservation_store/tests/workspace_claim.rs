//! [ORB-10709] The workspace claim shares `task_reservations` with the file
//! reservations, so these tests assert both halves of that bargain: the claim is
//! exclusive and TTL-bounded, and the two dimensions stay invisible to each
//! other.

use crate::Store;
use crate::TaskReservationReleaseParams;
use crate::TaskReservationReleaseReason;
use crate::TaskReservationReserveParams;
use crate::{WorkspaceClaimAcquireParams, WorkspaceClaimCheckParams, WorkspaceClaimReleaseParams};

fn acquire_params(actor: &str) -> WorkspaceClaimAcquireParams {
    WorkspaceClaimAcquireParams {
        workspace_orbit_dir: "/workspace/.orbit".to_string(),
        workspace_id: Some("repo-abcdef".to_string()),
        actor: actor.to_string(),
        ttl_seconds: 3600,
        machine_id: Some(format!("machine-{actor}")),
        session_id: Some(format!("session-{actor}")),
    }
}

fn check_params(claim_token: Option<&str>) -> WorkspaceClaimCheckParams {
    WorkspaceClaimCheckParams {
        workspace_orbit_dir: "/workspace/.orbit".to_string(),
        workspace_id: Some("repo-abcdef".to_string()),
        claim_token: claim_token.map(str::to_string),
    }
}

fn file_reserve_params(file: &str) -> TaskReservationReserveParams {
    TaskReservationReserveParams {
        workspace_orbit_dir: "/workspace/.orbit".to_string(),
        workspace_id: Some("repo-abcdef".to_string()),
        task_ids: vec!["ORB-00001".to_string()],
        requested_files: vec![file.to_string()],
        actor: "worker".to_string(),
        ttl_seconds: 3600,
        owner_run_id: None,
        owner_metadata_json: None,
    }
}

#[test]
fn second_operator_is_refused_with_the_incumbent_holder_and_expiry() {
    let store = Store::open_in_memory().expect("open store");

    let first = store
        .acquire_workspace_claim(&acquire_params("operator-a"))
        .expect("acquire first claim");
    assert!(first.acquired);
    let held = first.claim.expect("granted claim");

    let second = store
        .acquire_workspace_claim(&acquire_params("operator-b"))
        .expect("attempt second claim");
    assert!(!second.acquired, "contention must reject, never queue");
    assert!(
        second.claim_token.is_none(),
        "a refused contender must not learn the incumbent's token"
    );
    let conflict = second.conflict.expect("refusal names the incumbent");
    assert_eq!(conflict.actor, "operator-a");
    assert_eq!(conflict.expires_at, held.expires_at);
    assert_eq!(conflict.claim_id, held.claim_id);
    assert_eq!(conflict.machine_id.as_deref(), Some("machine-operator-a"));
    assert_eq!(conflict.session_id.as_deref(), Some("session-operator-a"));
}

#[test]
fn only_the_holders_token_satisfies_the_claim_check() {
    let store = Store::open_in_memory().expect("open store");
    let granted = store
        .acquire_workspace_claim(&acquire_params("operator-a"))
        .expect("acquire claim");
    let token = granted.claim_token.expect("minted token");

    let holder = store
        .check_workspace_claim(&check_params(Some(&token)))
        .expect("check with holder token");
    assert!(holder.token_matches);

    let stranger = store
        .check_workspace_claim(&check_params(Some("wsclaim-not-the-token")))
        .expect("check with a foreign token");
    assert!(!stranger.token_matches);
    assert_eq!(
        stranger.claim.expect("incumbent reported").actor,
        "operator-a"
    );

    let absent = store
        .check_workspace_claim(&check_params(None))
        .expect("check with no token");
    assert!(!absent.token_matches);
}

#[test]
fn an_unclaimed_workspace_reports_no_claim_and_no_match() {
    let store = Store::open_in_memory().expect("open store");

    let check = store
        .check_workspace_claim(&check_params(Some("wsclaim-anything")))
        .expect("check unclaimed workspace");
    assert!(check.claim.is_none());
    assert!(
        !check.token_matches,
        "an absent claim must be read as absent, not as a truthy match"
    );
}

#[test]
fn an_expired_claim_stops_blocking_without_intervention() {
    let store = Store::open_in_memory().expect("open store");
    let mut params = acquire_params("operator-a");
    params.ttl_seconds = 1;
    let granted = store
        .acquire_workspace_claim(&params)
        .expect("acquire short-lived claim");
    let expired_id = granted.claim.expect("granted claim").claim_id;

    std::thread::sleep(std::time::Duration::from_millis(1100));

    let check = store
        .check_workspace_claim(&check_params(None))
        .expect("check after expiry");
    assert!(check.claim.is_none(), "expired claim must stop gating");
    assert!(
        check
            .expired_claims
            .iter()
            .any(|expired| expired.reservation_id == expired_id),
        "lazy expiry reports what it released"
    );

    let successor = store
        .acquire_workspace_claim(&acquire_params("operator-b"))
        .expect("acquire after expiry");
    assert!(successor.acquired);
}

#[test]
fn force_release_records_who_forced_it_and_whom_they_displaced() {
    let store = Store::open_in_memory().expect("open store");
    let granted = store
        .acquire_workspace_claim(&acquire_params("operator-a"))
        .expect("acquire claim");
    let claim_id = granted.claim.expect("granted claim").claim_id;

    let refused = store
        .release_workspace_claim(&WorkspaceClaimReleaseParams {
            workspace_orbit_dir: "/workspace/.orbit".to_string(),
            workspace_id: Some("repo-abcdef".to_string()),
            claim_token: Some("wsclaim-wrong".to_string()),
            force: false,
            released_by: "operator-b".to_string(),
        })
        .expect("token release attempt");
    assert!(
        !refused.released,
        "a stale token must not release the claim"
    );
    assert_eq!(
        refused.claim.expect("incumbent reported").actor,
        "operator-a"
    );

    let forced = store
        .release_workspace_claim(&WorkspaceClaimReleaseParams {
            workspace_orbit_dir: "/workspace/.orbit".to_string(),
            workspace_id: Some("repo-abcdef".to_string()),
            claim_token: None,
            force: true,
            released_by: "operator-b".to_string(),
        })
        .expect("force release");
    assert!(forced.released);
    assert!(forced.forced);
    assert_eq!(
        forced.claim.expect("released claim").claim_id,
        claim_id,
        "the release names the displaced claim"
    );

    let after = store
        .show_workspace_claim("/workspace/.orbit", Some("repo-abcdef"))
        .expect("status after force release");
    assert!(after.claim.is_none());
}

#[test]
fn the_holder_releases_with_its_own_token_without_forcing() {
    let store = Store::open_in_memory().expect("open store");
    let granted = store
        .acquire_workspace_claim(&acquire_params("operator-a"))
        .expect("acquire claim");
    let token = granted.claim_token.expect("minted token");

    let released = store
        .release_workspace_claim(&WorkspaceClaimReleaseParams {
            workspace_orbit_dir: "/workspace/.orbit".to_string(),
            workspace_id: Some("repo-abcdef".to_string()),
            claim_token: Some(token),
            force: false,
            released_by: "operator-a".to_string(),
        })
        .expect("release with holder token");
    assert!(released.released);
    assert!(!released.forced, "a token release is not a force-release");
}

#[test]
fn claims_and_file_reservations_do_not_see_each_other() {
    let store = Store::open_in_memory().expect("open store");
    store
        .acquire_workspace_claim(&acquire_params("operator-a"))
        .expect("acquire claim");

    // A worker reservation is unaffected by an active workspace claim: the
    // claim gates dispatch, not paths.
    let reserved = store
        .reserve_task_reservation(&file_reserve_params("file:src/lib.rs"))
        .expect("reserve a file while the workspace is claimed");
    assert!(reserved.reserved, "claim must not block file reservations");
    assert!(reserved.conflicts.is_empty());
    let reservation_id = reserved.reservation_id.expect("reservation id");

    // ...and the claim is invisible to every file-reservation read path.
    let listed = store
        .list_active_task_reservations("/workspace/.orbit", Some("repo-abcdef"))
        .expect("list active reservations");
    assert_eq!(listed.reservations.len(), 1);
    assert_eq!(listed.reservations[0].reservation_id, reservation_id);

    let inspected = store
        .inspect_active_task_reservations("/workspace/.orbit", Some("repo-abcdef"))
        .expect("inspect active reservations");
    assert_eq!(inspected.len(), 1);

    // Releasing the file reservation leaves the claim standing.
    let released = store
        .release_task_reservation(&TaskReservationReleaseParams {
            workspace_orbit_dir: "/workspace/.orbit".to_string(),
            workspace_id: Some("repo-abcdef".to_string()),
            reservation_id,
            release_reason: TaskReservationReleaseReason::Explicit,
            release_metadata_json: None,
        })
        .expect("release file reservation");
    assert!(released.released);
    let claim = store
        .show_workspace_claim("/workspace/.orbit", Some("repo-abcdef"))
        .expect("claim status");
    assert_eq!(claim.claim.expect("claim still held").actor, "operator-a");
}

#[test]
fn a_claim_cannot_be_released_through_the_file_reservation_path() {
    let store = Store::open_in_memory().expect("open store");
    let granted = store
        .acquire_workspace_claim(&acquire_params("operator-a"))
        .expect("acquire claim");
    let claim_id = granted.claim.expect("granted claim").claim_id;

    let released = store
        .release_task_reservation(&TaskReservationReleaseParams {
            workspace_orbit_dir: "/workspace/.orbit".to_string(),
            workspace_id: Some("repo-abcdef".to_string()),
            reservation_id: claim_id,
            release_reason: TaskReservationReleaseReason::Explicit,
            release_metadata_json: None,
        })
        .expect("attempt to release the claim as a reservation");
    assert!(
        !released.released,
        "the file-reservation release path must not reach a claim row"
    );

    let claim = store
        .show_workspace_claim("/workspace/.orbit", Some("repo-abcdef"))
        .expect("claim status");
    assert!(claim.claim.is_some());
}

#[test]
fn claims_are_scoped_per_workspace() {
    let store = Store::open_in_memory().expect("open store");
    store
        .acquire_workspace_claim(&acquire_params("operator-a"))
        .expect("acquire claim in first workspace");

    let mut other = acquire_params("operator-b");
    other.workspace_id = Some("other-abcdef".to_string());
    other.workspace_orbit_dir = "/other/.orbit".to_string();
    let acquired = store
        .acquire_workspace_claim(&other)
        .expect("acquire claim in a different workspace");
    assert!(
        acquired.acquired,
        "a claim binds one workspace, not the whole host"
    );
}

#[cfg(unix)]
#[test]
fn persisted_workspace_claim_is_not_group_or_world_readable() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().expect("tempdir");
    let path = root.path().join("state/orbit.db");
    let store = Store::open(&path).expect("open file-backed store");
    let granted = store
        .acquire_workspace_claim(&acquire_params("operator-a"))
        .expect("persist workspace claim");
    assert!(
        granted.claim_token.is_some(),
        "claim persisted a bearer token"
    );

    for suffix in ["", "-wal", "-shm"] {
        let file = std::path::PathBuf::from(format!("{}{suffix}", path.display()));
        let mode = std::fs::metadata(&file)
            .expect("SQLite file metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode & 0o077,
            0,
            "{} must not grant group/other access (mode {mode:o})",
            file.display()
        );
    }
}
