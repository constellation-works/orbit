//! Publication transport is a separate workflow area. Its security fixture
//! owns a child process with a private PATH so no real transport can run.

#![cfg(unix)]
#![allow(clippy::expect_used, clippy::unwrap_used, missing_docs)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use orbit_common::{process, test_env};
use orbit_store::maintenance::task_registry::{TaskRegistryStore, task_registry_path};
use orbit_store::workflow::task::{
    PublicationInspectRequest, PublicationRestoreMode, PublicationRestoreRequest,
    inspect_publication, restore_publication,
};

#[test]
fn inspect_and_restore_refuse_credentials_before_git_or_cache_writes() {
    const TEST: &str = "inspect_and_restore_refuse_credentials_before_git_or_cache_writes";
    const CHILD: &str = "ORBIT_TEST_PUBLICATION_CREDENTIAL_CHILD";
    if std::env::var(CHILD).as_deref() != Ok(TEST) {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let git = bin.join("git");
        fs::write(
            &git,
            "#!/bin/sh\nprintf called > \"$ORBIT_TEST_GIT_MARKER\"\nprintf 'fixture transport refusal\\n' >&2\nexit 1\n",
        )
        .unwrap();
        fs::set_permissions(&git, fs::Permissions::from_mode(0o700)).unwrap();
        let mut command = Command::new(std::env::current_exe().unwrap());
        test_env::clear_inherited_authority(|key| {
            command.env_remove(key);
        });
        command
            .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
            .env(CHILD, TEST)
            .env("HOME", root.path())
            .env("USERPROFILE", root.path())
            .env("PATH", &bin)
            .env("ORBIT_TEST_GIT_MARKER", root.path().join("git-called"))
            .current_dir(root.path());
        let output = process::run_bounded_capped(&mut command, Duration::from_secs(60), 256 * 1024)
            .expect("run isolated publication fixture");
        test_env::assert_child_test_passed(TEST, output.status, output.stdout, output.stderr);
        return;
    }

    let root = std::env::current_dir().unwrap();
    let marker = root.join("git-called");
    let registry = TaskRegistryStore::open(&task_registry_path(&root.join("global"))).unwrap();
    for (case, remote) in [
        "https://publication-secret@host.test/repo.git",
        "http://publication-secret@host.test/repo.git",
        "https://user:publication-secret@host.test/repo.git",
        "ssh://user:publication-secret@host.test/repo.git",
        "https://user:publication-secret@host.test:invalid/repo.git",
        "https://publication-secret@host.test:invalid/repo.git",
        "https://publication-secret@host.test",
    ]
    .into_iter()
    .enumerate()
    {
        let cache = root.join(format!("refused-{case}"));
        let request = request(remote, &cache);
        let inspect_error = inspect_publication(request.clone()).unwrap_err();
        let restore_error = restore_publication(
            &registry,
            PublicationRestoreRequest {
                task_workspace_id: "ws_consumer".into(),
                publication: request,
                mode: PublicationRestoreMode::EmptyDestination,
            },
        )
        .unwrap_err();
        for error in [inspect_error, restore_error] {
            let error = error.to_string();
            assert!(error.contains("***@host.test"), "{error}");
            assert!(
                !error.contains("publication-secret"),
                "publication credential refusal must redact the token: {error}"
            );
        }
        assert!(!marker.exists(), "credential refusal must precede Git");
        assert!(
            !cache.exists(),
            "credential refusal must precede cache creation"
        );
        assert!(
            registry
                .tasks_for_workspace("ws_consumer")
                .unwrap()
                .is_empty()
        );
    }

    // A refusal alone could hide a broken fixture. Credential-free HTTPS,
    // SSH (including its transport username), SCP and local remotes reach Git.
    for (case, remote) in [
        "https://host.test/repo.git",
        "ssh://git@host.test/repo.git",
        "git@host.test:repo.git",
        root.to_str().unwrap(),
    ]
    .into_iter()
    .enumerate()
    {
        let error = inspect_publication(request(remote, &root.join(format!("allowed-{case}"))))
            .unwrap_err()
            .to_string();
        assert!(marker.is_file(), "credential-free remote must reach Git");
        assert!(error.contains("fixture transport refusal"), "{error}");
    }
}

fn request(remote: &str, cache: &Path) -> PublicationInspectRequest {
    PublicationInspectRequest {
        workspace_id: "ws_consumer".into(),
        source_repository_fingerprint: "ssh://source.test/orbit.git".into(),
        publication_id: "pub_consumer".into(),
        authority_machine_id: "hm_owner".into(),
        publication_remote: remote.into(),
        publication_branch: "missing".into(),
        cache_dir: cache.into(),
        commit: None,
    }
}
