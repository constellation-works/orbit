//! [ORB-10709] The persisted workspace claim carries a bearer token, so its
//! file must stay private to the owning user.

use super::*;
use orbit_store::Store;
use orbit_store::contracts::WorkspaceClaimAcquireParams;
use orbit_store::contracts::WorkspaceClaimCheckParams;
use orbit_store::contracts::WorkspaceClaimReleaseParams;

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

#[cfg(unix)]
#[test]
fn persisted_workspace_claim_is_not_group_or_world_readable() {
    if !support::isolated(
        "workspace_claim::persisted_workspace_claim_is_not_group_or_world_readable",
    ) {
        return;
    }
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

#[test]
fn workspace_claim_tokens_keep_legacy_format_and_validate_existing_claims() {
    if !support::isolated(
        "workspace_claim::workspace_claim_tokens_keep_legacy_format_and_validate_existing_claims",
    ) {
        return;
    }

    let root = tempfile::tempdir().expect("tempdir");
    let path = root.path().join("state/orbit.db");
    let store = Store::open(&path).expect("open file-backed store");
    let acquired = store
        .acquire_workspace_claim(&acquire_params("operator-a"))
        .expect("acquire workspace claim");
    let token = acquired.claim_token.expect("claim token");
    let encoded = token.strip_prefix("wsclaim-").expect("claim token prefix");
    assert_eq!(encoded.len(), 64, "claim token must encode 32 random bytes");
    assert!(
        encoded
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
        "claim token encoding must remain lowercase hexadecimal"
    );

    let check = |claim_token: &str| {
        store
            .check_workspace_claim(&WorkspaceClaimCheckParams {
                workspace_orbit_dir: "/workspace/.orbit".to_string(),
                workspace_id: Some("repo-abcdef".to_string()),
                claim_token: Some(claim_token.to_string()),
            })
            .expect("check workspace claim")
            .token_matches
    };
    assert!(check(&token), "the newly issued token must validate");

    let mut wrong_token = token.clone();
    let replacement = if wrong_token.as_bytes()[8] == b'0' {
        "1"
    } else {
        "0"
    };
    wrong_token.replace_range(8..9, replacement);
    assert!(!check(&wrong_token), "a different token must not validate");

    // Rows minted by the former generator used the same `wsclaim-` plus 64
    // lowercase hex format. Restore one such stored value and validate it
    // through the public check and release operations.
    let legacy_token = format!("wsclaim-{}", "0123456789abcdef".repeat(4));
    let claim_id = acquired.claim.expect("claim holder").claim_id;
    let connection = rusqlite::Connection::open(&path).expect("open claim database fixture");
    connection
        .execute(
            "UPDATE task_reservations SET claim_token = ?1 WHERE reservation_id = ?2",
            rusqlite::params![legacy_token, claim_id],
        )
        .expect("restore legacy-format stored token");
    drop(connection);

    assert!(
        check(&legacy_token),
        "a pre-change claim token must validate"
    );
    let released = store
        .release_workspace_claim(&WorkspaceClaimReleaseParams {
            workspace_orbit_dir: "/workspace/.orbit".to_string(),
            workspace_id: Some("repo-abcdef".to_string()),
            claim_token: Some(legacy_token),
            force: false,
            released_by: "operator-a".to_string(),
        })
        .expect("release workspace claim with legacy token");
    assert!(
        released.released,
        "a pre-change claim token must release its claim"
    );
}
