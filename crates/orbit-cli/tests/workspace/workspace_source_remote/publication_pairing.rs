//! Regression for legacy task pairings surviving a repository transfer.

use super::*;

const PUBLICATION_REMOTE: &str = "git@github.com:example/task-publication.git";
const TRANSFERRED_REMOTE: &str = "https://github.com/new-owner/orbit.git";

#[test]
fn source_transfer_updates_publication_pairing() {
    let test =
        "workspace_source_remote::publication_pairing::source_transfer_updates_publication_pairing";
    if crate::isolated_test::run_in_child(test) {
        return;
    }
    let scratch = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.orbit/tmp");
    let fixture =
        tempfile::tempdir_in(scratch.canonicalize().expect("canonical scratch")).expect("fixture");
    let home = fixture.path().join("home");
    let repo = fixture.path().join("repo");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&repo).unwrap();
    crate::git_repo::run(&repo, &["init"]);
    crate::git_repo::run(&repo, &["remote", "add", "origin", OLD_REMOTE]);
    initialize_workspace(&repo, &home, "publication-pairing-owner");
    bind(&repo, &home);
    remove(&repo, &home);
    run_json(
        &repo,
        &home,
        &[
            "workspace",
            "source-remote",
            "rebind",
            "--remote",
            TRANSFERRED_REMOTE,
            "--json",
        ],
    );
    assert_eq!(
        bind(&repo, &home)["source_repository_fingerprint"],
        TRANSFERRED_REMOTE
    );
    assert_eq!(
        run_json(
            &repo,
            &home,
            &["workspace", "publication", "show", "--json"]
        )["source_repository_fingerprint"],
        TRANSFERRED_REMOTE
    );
}

#[test]
fn source_transfer_repairs_legacy_publication_pairing() {
    let test = "workspace_source_remote::publication_pairing::source_transfer_repairs_legacy_publication_pairing";
    if crate::isolated_test::run_in_child(test) {
        return;
    }
    let scratch = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.orbit/tmp");
    let fixture =
        tempfile::tempdir_in(scratch.canonicalize().expect("canonical scratch")).expect("fixture");
    let home = fixture.path().join("home");
    let repo = fixture.path().join("repo");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(&repo).unwrap();
    crate::git_repo::run(&repo, &["init"]);
    crate::git_repo::run(&repo, &["remote", "add", "origin", OLD_REMOTE]);
    initialize_workspace(&repo, &home, "publication-transfer-owner");

    // Use the CLI to create the shipped raw-URL pairing, then remove only the
    // publication binding. The task-registry pairing remains persisted.
    bind(&repo, &home);
    remove(&repo, &home);
    let registry_path = home.join(".orbit/workspaces.json");
    let mut registry = read_json(&registry_path);
    // An upgraded registration may spell the same source with HTTPS. Keep the
    // task partition distinct from the logical workspace, as on older hosts.
    registry["workspaces"][0]["git_remote"] = "https://github.com/example/orbit.git".into();
    registry["workspaces"][0]["id"] = "ws_logical".into();
    registry["checkouts"][0]["workspace_id"] = "ws_logical".into();
    fs::write(&registry_path, serde_json::to_vec(&registry).unwrap()).unwrap();
    assert_eq!(bind(&repo, &home)["workspace_id"], "ws_logical");
    remove(&repo, &home);

    // A different identity is still a pairing conflict without an explicit
    // source rebind. This also models hosts already moved by an older binary.
    for unexpected in [TRANSFERRED_REMOTE, "https://github.com/example/other.git"] {
        registry["workspaces"][0]["git_remote"] = unexpected.into();
        fs::write(&registry_path, serde_json::to_vec(&registry).unwrap()).unwrap();
        orbit(&repo, &home)
            .args(bind_args())
            .assert()
            .failure()
            .stderr(predicate::str::contains(
                "different source-repository fingerprint",
            ));
    }
    registry["workspaces"][0]["git_remote"] = OLD_REMOTE.into();
    fs::write(&registry_path, serde_json::to_vec(&registry).unwrap()).unwrap();
    run_json(
        &repo,
        &home,
        &[
            "workspace",
            "source-remote",
            "rebind",
            "--remote",
            TRANSFERRED_REMOTE,
            "--dry-run",
            "--json",
        ],
    );
    assert_eq!(
        bind(&repo, &home)["source_repository_fingerprint"],
        OLD_REMOTE
    );
    // Existing publication lineage must be explicitly removed, never silently
    // carried into the transferred repository's identity.
    orbit(&repo, &home)
        .args([
            "workspace",
            "source-remote",
            "rebind",
            "--remote",
            TRANSFERRED_REMOTE,
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("workspace publication rebind"));
    remove(&repo, &home);
    run_json(
        &repo,
        &home,
        &[
            "workspace",
            "source-remote",
            "rebind",
            "--remote",
            TRANSFERRED_REMOTE,
            "--json",
        ],
    );
    assert_eq!(
        bind(&repo, &home)["source_repository_fingerprint"],
        TRANSFERRED_REMOTE
    );
    let shown = run_json(
        &repo,
        &home,
        &["workspace", "publication", "show", "--json"],
    );
    assert_eq!(shown["source_repository_fingerprint"], TRANSFERRED_REMOTE);
    assert!(shown["last_success_generation"].is_null());
    remove(&repo, &home);

    // Simulate an older source rebind leaving a stale task pairing. Repeating
    // the audited command for the registered URL must repair that state too.
    registry["workspaces"][0]["git_remote"] = NEW_REMOTE.into();
    fs::write(&registry_path, serde_json::to_vec(&registry).unwrap()).unwrap();
    let retried = run_json(
        &repo,
        &home,
        &[
            "workspace",
            "source-remote",
            "rebind",
            "--remote",
            NEW_REMOTE,
            "--json",
        ],
    );
    assert_eq!(retried["changed"], false);
    assert_eq!(
        bind(&repo, &home)["source_repository_fingerprint"],
        NEW_REMOTE
    );
}

fn bind_args() -> [&'static str; 8] {
    [
        "workspace",
        "publication",
        "bind",
        "--remote",
        PUBLICATION_REMOTE,
        "--publication-id",
        "pub_transfer",
        "--json",
    ]
}

fn bind(repo: &Path, home: &Path) -> Value {
    run_json(repo, home, &bind_args())
}

fn remove(repo: &Path, home: &Path) {
    orbit(repo, home)
        .args(["workspace", "publication", "remove", "--confirm"])
        .assert()
        .success();
}
