use std::fs;
use std::path::Path;

use super::super::WorktreeBoundaryGuard;
use super::super::boundary_guard::PrimaryBaseline;
use super::super::fingerprint::git_fingerprint;
use crate::activity_job::dispatcher::DispatchError;

/// ORB-23736: a stationary primary whose only dirt is Orbit's own generation
/// records must not fail as `primary_checkout_drift`, while a stationary
/// source path outside those records and outside `.orbit/` still must.
#[test]
fn stationary_primary_generation_records_are_benign() {
    let temp = tempfile::tempdir().expect("tempdir");
    let primary = temp.path().join("primary");
    let assigned = temp.path().join("assigned");
    init_repo(&primary);
    init_repo(&assigned);

    let primary_before = git_fingerprint(&primary).expect("primary before");
    let assigned_before = git_fingerprint(&assigned).expect("assigned before");
    let base_branch = primary_before.branch.clone().expect("primary has a branch");
    let head_before = primary_before.head.clone();

    fs::write(primary.join(".generation-compat.json"), "{}\n").expect("compat record");
    fs::write(primary.join(".generation-image-digest.json"), "{}\n").expect("digest cache");
    fs::create_dir_all(primary.join(".generation-participants")).expect("participants dir");
    fs::write(
        primary
            .join(".generation-participants")
            .join("1234-abcdef.json"),
        "{}\n",
    )
    .expect("participant record");

    let guard = WorktreeBoundaryGuard {
        task_id: "ORB-23736".to_string(),
        run_id: "jrun-test".to_string(),
        provider: "test".to_string(),
        requested_workspace_path: assigned.display().to_string(),
        requested_repo_root: Some(assigned.display().to_string()),
        assigned_root: assigned.clone(),
        primary_root: primary.clone(),
        assigned_before: assigned_before.clone(),
        primary_before: primary_before.clone(),
        primary_baseline: PrimaryBaseline {
            base_branch: base_branch.clone(),
            base_tip: Some(head_before.clone()),
        },
        rebase_recovery: None,
        activity: "implement_one".to_string(),
        audit: None,
    };

    guard
        .verify()
        .expect("ORB-23736: stationary generation-record dirt is benign");

    assert_head_unchanged(&primary, &head_before);
    assert_not_staged(&primary);
    assert_path_present(&primary.join(".generation-compat.json"));
    assert_path_present(&primary.join(".generation-image-digest.json"));
    assert_path_present(
        &primary
            .join(".generation-participants")
            .join("1234-abcdef.json"),
    );

    fs::write(primary.join("src-drift.txt"), "drift\n").expect("source dirt");

    let error = guard
        .verify()
        .expect_err("ORB-23736: stationary source dirt must still drift");
    match error {
        DispatchError::WorktreeIntegrity { code, diagnostic } => {
            assert_eq!(
                code, "primary_checkout_drift",
                "ORB-23736: source dirt outside generation records stays drift: {diagnostic}"
            );
            assert!(
                diagnostic.contains("src-drift.txt"),
                "ORB-23736: drift diagnostic names the source path: {diagnostic}"
            );
        }
        other => panic!("ORB-23736: expected primary_checkout_drift, got {other}"),
    }

    assert_head_unchanged(&primary, &head_before);
    assert_not_staged(&primary);
    assert_path_present(&primary.join(".generation-compat.json"));
    assert_path_present(&primary.join("src-drift.txt"));
}

fn init_repo(path: &Path) {
    fs::create_dir_all(path).expect("repo dir");
    git(path, &["init", "-b", "main"]);
    git(
        path,
        &["config", "user.email", "boundary-guard@example.test"],
    );
    git(path, &["config", "user.name", "boundary-guard"]);
    fs::write(path.join("README"), "hello\n").expect("readme");
    git(path, &["add", "README"]);
    git(path, &["commit", "-m", "init"]);
}

fn git(repo: &Path, args: &[&str]) {
    let output = crate::git_test_support::command()
        .arg("-C")
        .arg(repo)
        .args(["-c", "core.hooksPath=/dev/null"])
        .args(["-c", "commit.gpgsign=false"])
        .args(["-c", "gc.auto=0"])
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} in {} failed: {}",
        repo.display(),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_head_unchanged(repo: &Path, expected: &str) {
    let output = crate::git_test_support::command()
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "--verify", "HEAD"])
        .output()
        .expect("rev-parse HEAD");
    assert!(output.status.success(), "rev-parse HEAD must succeed");
    let head = String::from_utf8_lossy(&output.stdout).trim().to_string();
    assert_eq!(
        head, expected,
        "ORB-23736: the guard must not move, reset, or rewrite HEAD"
    );
}

fn assert_not_staged(repo: &Path) {
    let output = crate::git_test_support::command()
        .arg("-C")
        .arg(repo)
        .args(["diff", "--cached", "--name-only", "-z"])
        .output()
        .expect("staged names");
    assert!(output.status.success(), "staged check must succeed");
    assert!(
        output.stdout.is_empty(),
        "ORB-23736: the guard must not stage, commit, or clean either checkout"
    );
}

fn assert_path_present(path: &Path) {
    assert!(
        path.is_file(),
        "ORB-23736: the guard must not clean {}",
        path.display()
    );
}
