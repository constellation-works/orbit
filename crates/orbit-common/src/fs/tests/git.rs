use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

use super::super::git::{
    GIT_FETCH_CAS_ATTEMPTS, GIT_FETCH_LOCK_NAME, git_common_dir, git_fetch_lock_target,
    is_git_ref_update_contention, should_retry_git_ref_cas,
};

#[test]
fn cas_contention_matches_the_live_packed_refs_race() {
    let incident = "cannot lock ref 'refs/remotes/origin/agent-main': is at \
         1c140f6cd310394f59fc2a8639269d6bc4ca25f1 but expected \
         eb26940c037ce255b6c28c0378c9276ab38cc75e";
    assert!(is_git_ref_update_contention(incident));
    assert!(should_retry_git_ref_cas(0, incident));
    assert!(!should_retry_git_ref_cas(
        GIT_FETCH_CAS_ATTEMPTS - 1,
        incident
    ));
}

#[test]
fn lock_file_contention_is_retryable() {
    let stderr = "cannot lock ref 'refs/remotes/origin/agent-main': \
         Unable to create '.git/refs/remotes/origin/agent-main.lock': File exists.\n\
         error: unable to update local ref";
    assert!(is_git_ref_update_contention(stderr));
}

#[test]
fn auth_and_network_failures_are_not_retryable() {
    for stderr in [
        "fatal: Authentication failed for 'https://github.com/example/orbit.git/'",
        "ssh: Could not resolve host github.com\nfatal: Could not read from remote repository.",
        "fatal: unable to access 'https://github.com/example/orbit.git/': Could not resolve host",
        "fatal: could not read Username for 'https://github.com': terminal prompts disabled",
        "Permission denied (publickey).\nfatal: Could not read from remote repository.",
        "fatal: unable to access 'https://github.com/example/orbit.git/': The requested URL returned error: 403",
    ] {
        assert!(
            !is_git_ref_update_contention(stderr),
            "retried a real remote failure: {stderr}"
        );
        assert!(!should_retry_git_ref_cas(0, stderr), "{stderr}");
    }
}

#[test]
fn linked_worktrees_share_one_fetch_lock_target() {
    let fixture = LinkedWorktreeFixture::new();
    let primary_common = git_common_dir(&fixture.primary).expect("primary common dir");
    let linked_common = git_common_dir(&fixture.linked).expect("linked common dir");
    assert_eq!(primary_common, linked_common);
    assert_eq!(
        git_fetch_lock_target(&primary_common),
        primary_common.join(GIT_FETCH_LOCK_NAME)
    );
}

struct LinkedWorktreeFixture {
    _root: TempDir,
    primary: PathBuf,
    linked: PathBuf,
}

impl LinkedWorktreeFixture {
    fn new() -> Self {
        let root = TempDir::new().expect("temp dir");
        let primary = root.path().join("primary");
        let linked = root.path().join("linked");
        fs::create_dir_all(&primary).expect("primary dir");
        git(&primary, &["init"]);
        git(&primary, &["checkout", "-b", "agent-main"]);
        git(&primary, &["config", "user.name", "Orbit Test"]);
        git(
            &primary,
            &["config", "user.email", "orbit-test@example.com"],
        );
        git(&primary, &["config", "commit.gpgsign", "false"]);
        fs::write(primary.join("base.txt"), "v1\n").expect("write");
        git(&primary, &["add", "base.txt"]);
        git(&primary, &["commit", "-m", "init"]);
        git(
            &primary,
            &["worktree", "add", linked.to_str().unwrap(), "HEAD"],
        );
        Self {
            _root: root,
            primary,
            linked,
        }
    }
}

fn git(current_dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(current_dir)
        .output()
        .unwrap_or_else(|error| panic!("spawn git {}: {error}", args.join(" ")));
    assert!(
        output.status.success(),
        "git {} failed in {}:\nstdout: {}\nstderr: {}",
        args.join(" "),
        current_dir.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
