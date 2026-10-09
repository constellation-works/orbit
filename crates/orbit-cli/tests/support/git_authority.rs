//! Exercise real fixture entry points with Git authority confined to a decoy.

use std::fs;
use std::path::Path;
use std::process::Command;

use orbit_common::test_env;
use tempfile::tempdir;

use crate::git_repo;

/// Re-execute fixture tests without changing the suite process's environment.
/// Both all repository locators and `GIT_COMMON_DIR` alone must be harmless.
pub(crate) fn assert_fixtures_preserve_decoy(test_names: &[&str]) {
    let temp = tempdir().expect("Git authority fixture");
    let decoy = temp.path().join("decoy");
    git_repo::init(&decoy);
    for args in [
        vec!["config", "user.name", "Decoy"],
        vec!["config", "user.email", "decoy@example.invalid"],
        vec!["config", "commit.gpgsign", "false"],
        vec!["config", "core.hooksPath", "/dev/null"],
    ] {
        git(&decoy, &args);
    }
    fs::write(decoy.join("sentinel.txt"), "preserve the decoy\n").expect("decoy sentinel");
    git(&decoy, &["add", "sentinel.txt"]);
    git(&decoy, &["commit", "-m", "decoy baseline"]);
    let decoy_remote = temp.path().join("decoy-origin.git");
    fs::create_dir_all(&decoy_remote).expect("decoy remote directory");
    git(&decoy_remote, &["init", "--bare"]);
    git(
        &decoy,
        &["remote", "add", "origin", decoy_remote.to_str().unwrap()],
    );
    git(&decoy, &["push", "origin", "HEAD:refs/heads/main"]);
    let before = state(&decoy);
    let remote_before = git(&decoy_remote, &["for-each-ref"]);

    for common_only in [false, true] {
        for test_name in test_names {
            let output_dir = tempdir().expect("child output directory");
            let mut child = Command::new(std::env::current_exe().expect("test executable"));
            test_env::clear_inherited_authority(|name| {
                child.env_remove(name);
            });
            let git_dir = decoy.join(".git");
            child.env("GIT_COMMON_DIR", &git_dir);
            if !common_only {
                child
                    .env("GIT_DIR", &git_dir)
                    .env("GIT_WORK_TREE", &decoy)
                    .env("GIT_INDEX_FILE", git_dir.join("index"))
                    .env("GIT_OBJECT_DIRECTORY", git_dir.join("objects"))
                    .env("GIT_ALTERNATE_OBJECT_DIRECTORIES", git_dir.join("objects"));
            }
            child.args([
                "--exact",
                test_name,
                "--include-ignored",
                "--nocapture",
                "--test-threads=1",
            ]);
            let output = test_env::run_child_test(&mut child, test_name, output_dir.path());
            assert_eq!(
                state(&decoy),
                before,
                "{test_name} changed decoy HEAD, refs, config or index (common_only={common_only})"
            );
            assert_eq!(
                git(&decoy_remote, &["for-each-ref"]),
                remote_before,
                "{test_name} pushed to the decoy origin (common_only={common_only})"
            );
            test_env::assert_child_test_passed(
                test_name,
                output.status,
                &output.stdout,
                &output.stderr,
            );
        }
    }
}

fn state(repo: &Path) -> [Vec<u8>; 4] {
    [
        fs::read(repo.join(".git/HEAD")).expect("decoy HEAD"),
        git(repo, &["for-each-ref"]),
        fs::read(repo.join(".git/config")).expect("decoy config"),
        fs::read(repo.join(".git/index")).expect("decoy index"),
    ]
}

fn git(repo: &Path, args: &[&str]) -> Vec<u8> {
    let output = git_repo::command()
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("decoy Git command");
    assert!(
        output.status.success(),
        "decoy git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}
