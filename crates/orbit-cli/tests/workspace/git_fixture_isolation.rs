//! Shared fixture Git setup and reads must stay off an inherited repository.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use orbit_common::test_env;
use tempfile::tempdir;

use crate::git_repo;

#[test]
fn git_fixtures_preserve_inherited_authority_decoy() {
    const TEST: &str = "git_fixture_isolation::git_fixtures_preserve_inherited_authority_decoy";
    const CHILD_ROOT: &str = "ORBIT_TEST_GIT_AUTHORITY_ROOT";
    if let Some(root) = std::env::var_os(CHILD_ROOT) {
        exercise_shared_fixture(&PathBuf::from(root));
        return;
    }

    let temp = tempdir().expect("fixture tempdir");
    let decoy = temp.path().join("decoy");
    git_repo::SeededRepo::new(&decoy)
        .readme("preserve the decoy\n")
        .identity("Decoy Owner", "decoy@example.invalid")
        .build();
    git(&decoy, &["config", "core.hooksPath", "/dev/null"]);
    let remote = temp.path().join("decoy-origin.git");
    fs::create_dir(&remote).expect("decoy remote directory");
    git(&remote, &["init", "--bare", "--quiet"]);
    git(
        &decoy,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&decoy, &["push", "origin", "HEAD:refs/heads/main"]);
    let before = snapshot(&decoy);
    let remote_before = git(&remote, &["for-each-ref"]);

    // The same helper serves every CLI suite. Exercise both locator forms
    // directly instead of re-running their complete Orbit boundary fixtures.
    for common_only in [false, true] {
        let root = temp
            .path()
            .join(if common_only { "common-only" } else { "all" });
        let home = root.join("home");
        fs::create_dir_all(&home).expect("child home");
        let mut child = Command::new(std::env::current_exe().expect("workspace test binary"));
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
        child
            .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
            .env(CHILD_ROOT, &root)
            .env("HOME", &home)
            .env("USERPROFILE", &home)
            // Keep the deliberate locators if a pre-main scrub is added.
            .env(test_env::SCRUBBED_MARKER_ENV, "1");
        let output = test_env::run_child_test(&mut child, TEST, &root);
        assert_eq!(
            snapshot(&decoy),
            before,
            "shared fixture changed decoy config, HEAD, refs, index, worktrees or sentinel (common_only={common_only})"
        );
        assert_eq!(
            git(&remote, &["for-each-ref"]),
            remote_before,
            "shared fixture pushed to the decoy origin (common_only={common_only})"
        );
        test_env::assert_child_test_passed(TEST, output.status, &output.stdout, &output.stderr);
    }
}

fn exercise_shared_fixture(root: &Path) {
    assert!(std::env::var_os("GIT_COMMON_DIR").is_some());
    let repo = root.join("repo");
    git_repo::SeededRepo::new(&repo).build();
    assert_eq!(git_repo::run(&repo, &["branch", "--show-current"]), "main");
    assert_eq!(
        git_repo::run(&repo, &["rev-parse", "--show-toplevel"]),
        repo.canonicalize().unwrap().to_str().unwrap()
    );
    fs::write(repo.join("sentinel"), "fixture commit\n").expect("fixture sentinel");
    git_repo::run(&repo, &["add", "sentinel"]);
    git_repo::run(&repo, &["commit", "--quiet", "-m", "fixture"]);
    assert_eq!(git_repo::run(&repo, &["status", "--porcelain"]), "");
    fs::write(repo.join(".gitignore"), "ignored\n").expect("fixture ignore rule");
    assert_eq!(
        git_repo::run(&repo, &["check-ignore", "ignored"]),
        "ignored"
    );

    let remote = root.join("origin.git");
    fs::create_dir(&remote).expect("fixture remote directory");
    git_repo::run(&remote, &["init", "--bare", "--quiet"]);
    git_repo::run(
        &repo,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git_repo::run(&repo, &["push", "origin", "HEAD:refs/heads/main"]);
    assert_eq!(
        git_repo::run(&remote, &["rev-parse", "refs/heads/main"]),
        git_repo::run(&repo, &["rev-parse", "HEAD"])
    );
    let worktree = root.join("linked");
    git_repo::run(
        &repo,
        &[
            "worktree",
            "add",
            "-b",
            "linked",
            worktree.to_str().unwrap(),
        ],
    );
    assert_eq!(
        git_repo::run(&worktree, &["branch", "--show-current"]),
        "linked"
    );
    assert_eq!(git_repo::run(&worktree, &["status", "--porcelain"]), "");
    git_repo::run(&repo, &["worktree", "remove", worktree.to_str().unwrap()]);
    assert!(!worktree.exists());

    let sealed = root.join("sealed");
    git_repo::seal_lookup_boundary(&sealed);
    assert!(sealed.join(".orbit/config.toml").is_file());
    assert_eq!(
        git_repo::run(&sealed, &["branch", "--show-current"]),
        "main"
    );
    let checkout = git_repo::WorkCheckout::new_in(root);
    assert!(checkout.home.is_dir());
    assert!(checkout.work.join(".orbit/config.toml").is_file());
    assert_eq!(
        git_repo::run(&checkout.work, &["branch", "--show-current"]),
        "main"
    );
}

fn git(repo: &Path, args: &[&str]) -> Vec<u8> {
    let output = git_repo::command()
        .current_dir(repo)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(args)
        .output()
        .expect("run decoy Git command");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn snapshot(repo: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let git_dir = repo.join(".git");
    let mut snapshot = BTreeMap::from([
        (
            PathBuf::from("config"),
            fs::read(git_dir.join("config")).expect("decoy config"),
        ),
        (
            PathBuf::from("HEAD"),
            fs::read(git_dir.join("HEAD")).expect("decoy HEAD"),
        ),
        (
            PathBuf::from("index"),
            fs::read(git_dir.join("index")).expect("decoy index"),
        ),
        (
            PathBuf::from("worktree-list"),
            git(repo, &["worktree", "list", "--porcelain"]),
        ),
        (
            PathBuf::from("sentinel"),
            fs::read(repo.join("README.md")).expect("decoy sentinel"),
        ),
    ]);
    collect_files(&git_dir, &git_dir.join("refs"), &mut snapshot);
    collect_files(&git_dir, &git_dir.join("worktrees"), &mut snapshot);
    if git_dir.join("packed-refs").exists() {
        snapshot.insert(
            PathBuf::from("packed-refs"),
            fs::read(git_dir.join("packed-refs")).expect("decoy packed refs"),
        );
    }
    snapshot
}

fn collect_files(root: &Path, dir: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
    if !dir.exists() {
        return;
    }
    for entry in fs::read_dir(dir).expect("read decoy Git directory") {
        let entry = entry.expect("decoy Git entry");
        let path = entry.path();
        if entry.file_type().expect("decoy Git file type").is_dir() {
            collect_files(root, &path, files);
        } else {
            files.insert(
                path.strip_prefix(root)
                    .expect("decoy relative path")
                    .to_path_buf(),
                fs::read(path).expect("decoy Git file"),
            );
        }
    }
}
