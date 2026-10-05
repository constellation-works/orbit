//! [ORB-10709] The persisted workspace claim carries a bearer token, so its
//! file must stay private to the owning user.

use crate::Store;
use crate::WorkspaceClaimAcquireParams;

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
