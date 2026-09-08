#![allow(missing_docs)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use chrono::Utc;
use orbit_common::{NotFoundKind, OrbitError};
use orbit_tools::ToolContext;
use orbit_types::policy::Role;
use orbit_types::record::OrbitEvent;
use orbit_types::task::{ExternalRef, Task, TaskArtifact, TaskPriority, TaskStatus, TaskType};
use orbit_types::workflow::JobRun;
use tempfile::tempdir;

use serde_json::{Value, json};

use crate::context::{RuntimeHost, TaskActivityUpdate, TaskAutomationUpdate};

#[cfg(unix)]
use crate::executor::automation::vcs::tests::with_fake_git;

use super::super::super::commit::git_commit;
use super::super::resolve_worktree_path_from_prefix;
use super::super::setup::{ensure_worktree, setup_worktree, worktree_setup_output};

#[test]
fn ensure_worktree_refuses_registered_checkout_whose_head_is_not_the_new_base() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    let worktree = temp.path().join("worktree");
    init_repo(&repo, "agent-main");
    let first_base = commit_file(&repo, "base.txt", "v1");

    assert_eq!(
        ensure_worktree(&repo, &worktree, &first_base, "orbit/test").unwrap(),
        "orbit/test"
    );
    assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), first_base);

    let epic_commit = commit_file(&worktree, "child.txt", "landed");

    let second_base = commit_file(&repo, "base.txt", "v2");
    let error = ensure_worktree(&repo, &worktree, &second_base, "orbit/new-name").unwrap_err();

    assert_stale_branch_refusal(&error, "orbit/test", &epic_commit, &second_base);
    assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), epic_commit);
    assert_eq!(
        git(&worktree, &["symbolic-ref", "--short", "HEAD"]),
        "orbit/test"
    );
}

#[test]
fn ensure_worktree_reattaches_existing_checkout_at_the_same_base() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    let worktree = temp.path().join("worktree");
    init_repo(&repo, "agent-main");
    let first_base = commit_file(&repo, "base.txt", "v1");

    assert_eq!(
        ensure_worktree(&repo, &worktree, &first_base, "orbit/test").unwrap(),
        "orbit/test"
    );
    let reattached = ensure_worktree(&repo, &worktree, &first_base, "orbit/new-name").unwrap();

    assert_eq!(reattached, "orbit/test");
    assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), first_base);
}

/// A checkout that is a git work tree but not one this repository registered
/// is refused and left untouched: `clean -fd` must never run on it.
#[test]
fn ensure_worktree_refuses_to_clean_a_foreign_checkout_at_the_resolved_path() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    let worktree = temp.path().join("worktree");
    init_repo(&repo, "agent-main");
    let base = commit_file(&repo, "base.txt", "v1");
    // Someone else's repository lives exactly where this run's worktree would.
    init_repo(&worktree, "main");
    commit_file(&worktree, "theirs.txt", "tracked");
    fs::write(worktree.join("scratch.txt"), "untracked work").unwrap();

    let error = ensure_worktree(&repo, &worktree, &base, "orbit/test").unwrap_err();
    assert!(
        error
            .to_string()
            .contains("not a worktree of this repository"),
        "{error}"
    );
    assert_eq!(
        fs::read_to_string(worktree.join("scratch.txt")).unwrap(),
        "untracked work"
    );
}

#[test]
fn ensure_worktree_refuses_orphan_branch_whose_tip_is_not_the_new_base() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    let worktree = temp.path().join("worktree");
    init_repo(&repo, "agent-main");
    let first_base = commit_file(&repo, "base.txt", "v1");
    git(&repo, &["branch", "orbit/test", &first_base]);

    let second_base = commit_file(&repo, "base.txt", "v2");
    let error = ensure_worktree(&repo, &worktree, &second_base, "orbit/test").unwrap_err();

    assert_stale_branch_refusal(&error, "orbit/test", &first_base, &second_base);
    assert!(
        !worktree.exists(),
        "refused orphan attach must not create a worktree"
    );
    assert_eq!(git(&repo, &["rev-parse", "orbit/test"]), first_base);
}

#[test]
fn ensure_worktree_reuses_orphan_branch_at_the_same_base() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    let worktree = temp.path().join("worktree");
    init_repo(&repo, "agent-main");
    let first_base = commit_file(&repo, "base.txt", "v1");
    git(&repo, &["branch", "orbit/test", &first_base]);

    ensure_worktree(&repo, &worktree, &first_base, "orbit/test").unwrap();

    assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), first_base);
}

#[test]
fn ensure_worktree_prunes_dangling_metadata_from_failed_attempt() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    let worktree = temp.path().join("worktree");
    init_repo(&repo, "agent-main");
    let base = commit_file(&repo, "base.txt", "v1");

    ensure_worktree(&repo, &worktree, &base, "orbit/test").unwrap();
    fs::remove_dir_all(&worktree).unwrap();

    ensure_worktree(&repo, &worktree, &base, "orbit/test").unwrap();

    assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), base);
}

#[test]
fn ensure_worktree_reuses_empty_path_from_failed_attempt() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    let worktree = temp.path().join("worktree");
    init_repo(&repo, "agent-main");
    let base = commit_file(&repo, "base.txt", "v1");
    fs::create_dir_all(&worktree).unwrap();

    ensure_worktree(&repo, &worktree, &base, "orbit/test").unwrap();

    assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), base);
}

#[test]
fn ensure_worktree_uses_commit_start_point_without_upstream_config() {
    let temp = tempdir().unwrap();
    let remote = temp.path().join("remote.git");
    let seed = temp.path().join("seed");
    let local = temp.path().join("local");
    let worktree = temp.path().join("worktree");

    git(temp.path(), &["init", "--bare", remote.to_str().unwrap()]);
    init_repo(&seed, "agent-main");
    let remote_head = commit_file(&seed, "base.txt", "v1");
    git(
        &seed,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&seed, &["push", "-u", "origin", "agent-main"]);
    git(
        temp.path(),
        &[
            "clone",
            "--branch",
            "agent-main",
            remote.to_str().unwrap(),
            local.to_str().unwrap(),
        ],
    );

    ensure_worktree(&local, &worktree, "origin/agent-main", "orbit/test").unwrap();

    assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), remote_head);
    assert_git_fails(&local, &["config", "--get", "branch.orbit/test.remote"]);
    assert_git_fails(&local, &["config", "--get", "branch.orbit/test.merge"]);
}

#[test]
fn worktree_setup_output_includes_legacy_batch_id_alias() {
    let output = worktree_setup_output(
        "jrun-test",
        "/tmp/orbit-worktree".to_string(),
        "orbit/ORB-00010".to_string(),
        "main".to_string(),
        "1111111111111111111111111111111111111111".to_string(),
    );

    assert_eq!(output["job_run_id"], json!("jrun-test"));
    assert_eq!(output["batch_id"], output["job_run_id"]);
}

#[test]
fn worktree_setup_publishes_the_resolved_base_commit_alongside_the_moving_ref() {
    // ORB-10380: downstream steps must be able to pin the base this worktree was
    // created at, because `origin/<base>` moves while the run is in flight.
    let output = worktree_setup_output(
        "jrun-test",
        "/tmp/orbit-worktree".to_string(),
        "orbit/ORB-10380".to_string(),
        "origin/agent-main".to_string(),
        "2222222222222222222222222222222222222222".to_string(),
    );

    assert_eq!(output["base_ref"], json!("origin/agent-main"));
    assert_eq!(
        output["base_sha"],
        json!("2222222222222222222222222222222222222222")
    );
}

#[test]
fn completeness_leaves_deletions_dirty_files_and_retained_commits_intact() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    let worktree = temp.path().join("worktree");
    init_repo(&repo, "agent-main");
    commit_file(&repo, "base.txt", "v1");
    let first_base = commit_file(&repo, "keep.txt", "tracked");

    assert_eq!(
        ensure_worktree(&repo, &worktree, &first_base, "orbit/test").unwrap(),
        "orbit/test"
    );
    let retained = commit_file(&worktree, "child.txt", "landed");
    fs::remove_file(worktree.join("keep.txt")).unwrap();
    fs::write(worktree.join("dirty.txt"), "untracked work").unwrap();

    let second_base = commit_file(&repo, "base.txt", "v2");
    let error = ensure_worktree(&repo, &worktree, &second_base, "orbit/new-name").unwrap_err();

    assert_stale_branch_refusal(&error, "orbit/test", &retained, &second_base);
    assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), retained);
    assert!(
        !worktree.join("keep.txt").exists(),
        "tracked deletion must not be restored by completeness or provenance"
    );
    assert_eq!(
        fs::read_to_string(worktree.join("dirty.txt")).unwrap(),
        "untracked work"
    );
    assert_eq!(
        fs::read_to_string(worktree.join("child.txt")).unwrap(),
        "landed"
    );
}

#[test]
fn ensure_worktree_reuses_matching_checkout_without_cleaning_dirty_files() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    let worktree = temp.path().join("worktree");
    init_repo(&repo, "agent-main");
    commit_file(&repo, "base.txt", "v1");
    let first_base = commit_file(&repo, "keep.txt", "tracked");

    assert_eq!(
        ensure_worktree(&repo, &worktree, &first_base, "orbit/test").unwrap(),
        "orbit/test"
    );
    fs::remove_file(worktree.join("keep.txt")).unwrap();
    fs::write(worktree.join("dirty.txt"), "untracked work").unwrap();

    let reattached = ensure_worktree(&repo, &worktree, &first_base, "orbit/new-name").unwrap();

    assert_eq!(reattached, "orbit/test");
    assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), first_base);
    assert!(
        !worktree.join("keep.txt").exists(),
        "tracked deletion must not be restored when HEAD already matches the base"
    );
    assert_eq!(
        fs::read_to_string(worktree.join("dirty.txt")).unwrap(),
        "untracked work"
    );
}

#[test]
fn ensure_worktree_does_not_reset_a_pushed_retained_candidate() {
    let temp = tempdir().unwrap();
    let remote = temp.path().join("remote.git");
    let repo = temp.path().join("repo");
    let worktree = temp.path().join("worktree");
    git(temp.path(), &["init", "--bare", remote.to_str().unwrap()]);
    init_repo(&repo, "agent-main");
    let first_base = commit_file(&repo, "base.txt", "v1");
    git(
        &repo,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );
    git(&repo, &["push", "-u", "origin", "agent-main"]);

    ensure_worktree(&repo, &worktree, &first_base, "orbit/test").unwrap();
    let retained = commit_file(&worktree, "candidate.txt", "keep me");
    git(&worktree, &["push", "-u", "origin", "orbit/test"]);

    let second_base = commit_file(&repo, "base.txt", "v2");
    git(&repo, &["push", "origin", "agent-main"]);
    let error = ensure_worktree(&repo, &worktree, &second_base, "orbit/test").unwrap_err();

    assert_stale_branch_refusal(&error, "orbit/test", &retained, &second_base);
    assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), retained);
    assert_eq!(
        fs::read_to_string(worktree.join("candidate.txt")).unwrap(),
        "keep me"
    );
    assert_eq!(git(&repo, &["rev-parse", "origin/orbit/test"]), retained);
}

#[cfg(unix)]
#[test]
fn worktree_add_timeout_after_registration_is_not_admitted_on_retry() {
    let stamp_dir = tempdir().unwrap();
    let stamp = stamp_dir.path().join("worktree-once");
    if !with_fake_git(
        module_path!(),
        "worktree_add_timeout_after_registration_is_not_admitted_on_retry",
        &[("ORBIT_TEST_GIT_WORKTREE_ONCE", stamp.display().to_string())],
    ) {
        return;
    }

    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo, "agent-main");
    commit_file(&repo, "base.txt", "v1");
    let host = FakeHost::new(&repo, &["ORB-11606"]);
    let run_id = "jrun-timeout-worktree";
    let input = json!({
        "task_ids": ["ORB-11606"],
        "run_id": run_id,
        "base": "agent-main",
        "base_sync": "local",
        "dependency_delivery": "ignore",
        "git_timeouts": { "worktree_add": 400 },
    });

    let first = setup_worktree(&host, &input).expect_err("first add must time out");
    let first_message = first.to_string();
    assert!(
        first_message.contains("timed out"),
        "expected timeout diagnostic, got {first_message}"
    );
    assert!(
        first_message.contains("timeout recovery") || first_message.contains("Timeout recovery"),
        "timeout recovery must be named: {first_message}"
    );
    assert!(
        !first_message.contains("unresolved conflict"),
        "timeout must not be labeled a conflict: {first_message}"
    );
    assert!(
        host.admitted().is_empty(),
        "timed-out setup must not admit the task"
    );

    let worktree_path = resolve_worktree_path_from_prefix(&repo, "orbit", run_id).unwrap();
    let second = setup_worktree(&host, &input);
    match second {
        Ok(_) => {
            assert!(
                git_ok(&worktree_path, &["rev-parse", "--verify", "HEAD^{commit}"]),
                "retry may admit only a complete checkout"
            );
            assert_eq!(host.admitted(), vec!["ORB-11606".to_string()]);
        }
        Err(error) => {
            let message = error.to_string();
            assert!(
                host.admitted().is_empty(),
                "retry must not admit an incomplete checkout"
            );
            assert!(
                message.contains("incomplete") || message.contains("leaving checkout"),
                "refusal must preserve evidence: {message}"
            );
            assert!(
                worktree_path.exists(),
                "quarantined checkout must remain for inspection"
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn worktree_add_stderr_timeout_phrase_is_ordinary_failure_not_timeout_recovery() {
    if !with_fake_git(
        module_path!(),
        "worktree_add_stderr_timeout_phrase_is_ordinary_failure_not_timeout_recovery",
        &[("ORBIT_TEST_GIT_PHRASE_FAIL", "worktree".to_string())],
    ) {
        return;
    }

    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo, "agent-main");
    commit_file(&repo, "base.txt", "v1");
    let host = FakeHost::new(&repo, &["ORB-11802"]);
    let run_id = "jrun-phrase-worktree";
    let input = json!({
        "task_ids": ["ORB-11802"],
        "run_id": run_id,
        "base": "agent-main",
        "base_sync": "local",
        "dependency_delivery": "ignore",
    });

    let error = setup_worktree(&host, &input)
        .expect_err("injected stderr phrase must fail as ordinary Git");
    let message = error.to_string();
    assert!(
        message.contains("failed in"),
        "expected ordinary Git failure, got {message}"
    );
    assert!(
        !message.contains("timed out after"),
        "stderr phrase must not be a deadline error: {message}"
    );
    assert!(
        !message.contains("timeout recovery") && !message.contains("Timeout recovery"),
        "timeout recovery must not run: {message}"
    );
    assert!(
        host.admitted().is_empty(),
        "ordinary worktree add failure must not admit the task"
    );
}

#[test]
fn setup_worktree_refuses_stale_registered_checkout_before_admission() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo, "agent-main");
    let first_base = commit_file(&repo, "base.txt", "v1");
    let run_id = "jrun-stale-setup";
    let worktree_path = resolve_worktree_path_from_prefix(&repo, "orbit", run_id).unwrap();
    ensure_worktree(&repo, &worktree_path, &first_base, "orbit/test").unwrap();
    let retained = commit_file(&worktree_path, "child.txt", "landed");
    let second_base = commit_file(&repo, "base.txt", "v2");

    let host = FakeHost::new(&repo, &["ORB-11639"]);
    let input = json!({
        "task_ids": ["ORB-11639"],
        "run_id": run_id,
        "base": "agent-main",
        "base_sync": "local",
        "dependency_delivery": "ignore",
    });

    let error = setup_worktree(&host, &input).unwrap_err();
    assert_stale_branch_refusal(&error, "orbit/test", &retained, &second_base);
    assert!(
        host.admitted().is_empty(),
        "stale reuse must fail before workflow admission"
    );
    assert_eq!(git(&worktree_path, &["rev-parse", "HEAD"]), retained);
}

#[test]
fn stale_setup_retry_stays_refused_until_operator_recovers_the_branch() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo, "agent-main");
    let first_base = commit_file(&repo, "base.txt", "v1");
    let run_id = "jrun-stale-retry";
    let worktree_path = resolve_worktree_path_from_prefix(&repo, "orbit", run_id).unwrap();
    ensure_worktree(&repo, &worktree_path, &first_base, "orbit/test").unwrap();
    let retained = commit_file(&worktree_path, "candidate.txt", "inspect me");
    let second_base = commit_file(&repo, "base.txt", "v2");

    let host = FakeHost::new(&repo, &["ORB-11639"]);
    let input = json!({
        "task_ids": ["ORB-11639"],
        "run_id": run_id,
        "base": "agent-main",
        "base_sync": "local",
        "dependency_delivery": "ignore",
    });

    let first = setup_worktree(&host, &input).unwrap_err();
    assert_stale_branch_refusal(&first, "orbit/test", &retained, &second_base);
    assert!(host.admitted().is_empty());
    assert_eq!(git(&worktree_path, &["rev-parse", "HEAD"]), retained);

    let retry = setup_worktree(&host, &input).unwrap_err();
    assert_stale_branch_refusal(&retry, "orbit/test", &retained, &second_base);
    assert!(
        host.admitted().is_empty(),
        "setup must not auto-succeed on retry while the stale checkout remains"
    );
    assert_eq!(
        fs::read_to_string(worktree_path.join("candidate.txt")).unwrap(),
        "inspect me"
    );

    git(
        &repo,
        &[
            "worktree",
            "remove",
            "--force",
            worktree_path.to_str().unwrap(),
        ],
    );
    git(&repo, &["branch", "-D", "orbit/test"]);

    let output =
        setup_worktree(&host, &input).expect("operator recovery can recreate at the new base");
    assert_eq!(host.admitted(), vec!["ORB-11639".to_string()]);
    assert_eq!(output["base_sha"], json!(second_base));
    let workspace = PathBuf::from(output["workspace_path"].as_str().expect("workspace_path"));
    assert_eq!(git(&workspace, &["rev-parse", "HEAD"]), second_base);

    fs::write(workspace.join("task.txt"), "recovered work\n").unwrap();
    let commit = git_commit(
        &host,
        &json!({
            "scope": "all",
            "job_run_id": run_id,
            "workspace_path": workspace,
            "base_ref": output["base_ref"],
            "base_sha": output["base_sha"],
        }),
    )
    .expect("commit accepts a setup checkpoint whose HEAD still equals base_sha");
    assert_eq!(commit["decision"], json!("performed"));
    assert_eq!(commit["base_sha"], json!(second_base));
}

fn assert_stale_branch_refusal(error: &OrbitError, branch: &str, tip: &str, base: &str) {
    let message = error.to_string();
    assert!(
        message.contains("refusing stale branch"),
        "expected a stale-branch refusal, got {message}"
    );
    assert!(
        message.contains(&format!("'{branch}'")),
        "must name branch {branch}: {message}"
    );
    assert!(message.contains(tip), "must name tip {tip}: {message}");
    assert!(
        message.contains(base),
        "must name requested base {base}: {message}"
    );
    assert!(
        message.contains("retrying setup without that recovery will refuse again"),
        "must describe recovery, got {message}"
    );
}

fn git_ok(current_dir: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .args(args)
        .current_dir(current_dir)
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn init_repo(path: &Path, branch: &str) {
    fs::create_dir_all(path).unwrap();
    git(path, &["init"]);
    git(path, &["checkout", "-b", branch]);
    git(path, &["config", "user.name", "Orbit Test"]);
    git(path, &["config", "user.email", "orbit-test@example.com"]);
}

fn commit_file(repo: &Path, file_name: &str, contents: &str) -> String {
    fs::write(repo.join(file_name), contents).unwrap();
    git(repo, &["add", file_name]);
    git(repo, &["commit", "-m", &format!("write {file_name}")]);
    git(repo, &["rev-parse", "HEAD"])
}

fn git(current_dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(current_dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {} failed in {}:\nstdout: {}\nstderr: {}",
        args.join(" "),
        current_dir.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn assert_git_fails(current_dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(current_dir)
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "git {} unexpectedly succeeded in {}:\nstdout: {}\nstderr: {}",
        args.join(" "),
        current_dir.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn setup_worktree_refuses_dirty_base_checkout_when_landing_mode_is_local() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo, "agent-main");
    commit_file(&repo, "base.txt", "v1");

    // Introduce dirty landing state in the base repository checkout (simulating init generated files)
    fs::write(repo.join(".gitignore"), ".orbit/*\n").unwrap();
    fs::create_dir_all(repo.join(".orbit").join("auto_tasks")).unwrap();
    fs::write(
        repo.join(".orbit").join("auto_tasks").join("curation.yaml"),
        "enabled: false\n",
    )
    .unwrap();

    let host = FakeHost::new(&repo, &["ORB-11373"]);
    let run_id = "jrun-dirty-local-test";
    let input = json!({
        "task_ids": ["ORB-11373"],
        "run_id": run_id,
        "base": "agent-main",
        "base_sync": "local",
        "landing_mode": "local",
        "dependency_delivery": "ignore",
    });

    let error = setup_worktree(&host, &input).unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains("base branch checkout")
            && message.contains("must be clean before merge_batch_worktree_into_base"),
        "expected clean base checkout refusal, got: {message}"
    );

    // Verify refusal prevented worktree creation and task admission
    let worktree_path = resolve_worktree_path_from_prefix(&repo, "orbit", run_id).unwrap();
    assert!(
        !worktree_path.exists(),
        "refused setup must not leave a worktree behind"
    );
    assert!(
        host.admitted().is_empty(),
        "refused setup must not admit the task into workflow"
    );
}

#[test]
fn setup_worktree_allows_dirty_base_checkout_when_landing_mode_is_pr_or_omitted() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo, "agent-main");
    commit_file(&repo, "base.txt", "v1");

    // Introduce dirty state in base checkout
    fs::write(repo.join(".gitignore"), ".orbit/*\n").unwrap();

    let host = FakeHost::new(&repo, &["ORB-11373"]);
    let run_id = "jrun-dirty-pr-test";
    let input = json!({
        "task_ids": ["ORB-11373"],
        "run_id": run_id,
        "base": "agent-main",
        "base_sync": "local",
        "landing_mode": "pr",
        "dependency_delivery": "ignore",
    });

    let output = setup_worktree(&host, &input).expect("PR mode must allow dirty base checkout");
    assert_eq!(output["job_run_id"], json!(run_id));
    assert_eq!(host.admitted(), vec!["ORB-11373".to_string()]);
}

#[test]
fn setup_worktree_succeeds_when_landing_mode_is_local_and_base_is_clean() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo, "agent-main");
    commit_file(&repo, "base.txt", "v1");

    let host = FakeHost::new(&repo, &["ORB-11373"]);
    let run_id = "jrun-clean-local-test";
    let input = json!({
        "task_ids": ["ORB-11373"],
        "run_id": run_id,
        "base": "agent-main",
        "base_sync": "local",
        "landing_mode": "local",
        "dependency_delivery": "ignore",
    });

    let output =
        setup_worktree(&host, &input).expect("Clean base checkout must succeed in local mode");
    assert_eq!(output["job_run_id"], json!(run_id));
    assert_eq!(host.admitted(), vec!["ORB-11373".to_string()]);
}

#[test]
fn setup_worktree_keeps_an_explicit_identity_token_and_stamps_the_admitted_job() {
    let temp = tempdir().unwrap();
    let repo = temp.path().join("repo");
    init_repo(&repo, "agent-main");
    commit_file(&repo, "base.txt", "v1");

    let host = FakeHost::new(&repo, &["ORB-EPIC"]);
    let worktree_token = "epic-ORB-EPIC";
    let admitted_run = "jrun-admitted-epic";
    let input = json!({
        "task_ids": ["ORB-EPIC"],
        "run_id": worktree_token,
        "job_run_id": admitted_run,
        "branch_prefix": "epic",
        "base": "agent-main",
        "base_sync": "local",
        "dependency_delivery": "ignore",
    });

    let output = setup_worktree(&host, &input).expect("epic identity split");
    let worktree_path = resolve_worktree_path_from_prefix(&repo, "epic", worktree_token).unwrap();

    assert!(
        worktree_path.exists(),
        "the stable epic token still names the checkout"
    );
    assert_eq!(output["job_run_id"], json!(admitted_run));
    assert_eq!(output["batch_id"], json!(admitted_run));
    assert_eq!(
        host.stamped_job_run_id("ORB-EPIC").as_deref(),
        Some(admitted_run)
    );
}

struct FakeHost {
    tasks: BTreeMap<String, Task>,
    repo_root: PathBuf,
    data_root: PathBuf,
    scoreboard_dir: PathBuf,
    admitted: Mutex<Vec<String>>,
    stamped_job_run_id: Mutex<BTreeMap<String, String>>,
}

impl FakeHost {
    fn new(repo_root: &Path, task_ids: &[&str]) -> Self {
        let now = Utc::now();
        let tasks = task_ids
            .iter()
            .map(|id| {
                (
                    id.to_string(),
                    Task {
                        id: id.to_string(),
                        title: format!("Task {id}"),
                        description: String::new(),
                        acceptance_criteria: Vec::new(),
                        tags: Vec::new(),
                        required_tools: Vec::new(),
                        plan: String::new(),
                        execution_summary: String::new(),
                        context_files: Vec::new(),
                        created_by: None,
                        planned_by: None,
                        implemented_by: None,
                        status: TaskStatus::Backlog,
                        priority: TaskPriority::Medium,
                        complexity: None,
                        task_type: TaskType::Chore,
                        pr_status: None,
                        external_refs: Vec::new(),
                        relations: Vec::new(),
                        job_run_id: None,
                        crew: None,
                        orchestrator: None,
                        created_at: now,
                        updated_at: now,
                    },
                )
            })
            .collect();
        Self {
            tasks,
            repo_root: repo_root.to_path_buf(),
            data_root: repo_root.join(".orbit-test-data"),
            scoreboard_dir: repo_root.join(".orbit-test-data").join("scoreboard"),
            admitted: Mutex::new(Vec::new()),
            stamped_job_run_id: Mutex::new(BTreeMap::new()),
        }
    }

    fn admitted(&self) -> Vec<String> {
        self.admitted.lock().expect("admitted lock").clone()
    }

    fn stamped_job_run_id(&self, task_id: &str) -> Option<String> {
        self.stamped_job_run_id
            .lock()
            .expect("stamp lock")
            .get(task_id)
            .cloned()
    }
}

impl RuntimeHost for FakeHost {
    fn get_task(&self, task_id: &str) -> Result<Task, OrbitError> {
        self.tasks
            .get(task_id)
            .cloned()
            .ok_or_else(|| OrbitError::not_found(NotFoundKind::Task, task_id.to_string()))
    }

    fn get_task_artifacts(&self, _task_id: &str) -> Result<Vec<TaskArtifact>, OrbitError> {
        Ok(Vec::new())
    }

    fn list_tasks_filtered(
        &self,
        _status: Option<TaskStatus>,
        _priority: Option<TaskPriority>,
        _parent_id: Option<&str>,
        _job_run_id: Option<&str>,
        _external_ref: Option<&ExternalRef>,
        _has_external_ref_system: Option<&str>,
    ) -> Result<Vec<Task>, OrbitError> {
        Ok(self.tasks.values().cloned().collect())
    }

    fn start_task(
        &self,
        _task_id: &str,
        _note: Option<String>,
        _comment: Option<String>,
    ) -> Result<Task, OrbitError> {
        Err(OrbitError::Execution(
            "start_task not needed in setup tests".to_string(),
        ))
    }

    fn admit_task_for_workflow(&self, task_id: &str, _workflow: &str) -> Result<Task, OrbitError> {
        self.admitted
            .lock()
            .expect("admitted lock")
            .push(task_id.to_string());
        self.get_task(task_id)
    }

    fn update_task_from_activity(
        &self,
        _task_id: &str,
        _update: TaskActivityUpdate,
    ) -> Result<Task, OrbitError> {
        Err(OrbitError::Execution(
            "update_task_from_activity not needed in setup tests".to_string(),
        ))
    }

    fn apply_task_automation_update(
        &self,
        task_id: &str,
        update: TaskAutomationUpdate,
    ) -> Result<(), OrbitError> {
        if let Some(job_run_id) = update.job_run_id {
            self.stamped_job_run_id
                .lock()
                .expect("stamp lock")
                .insert(task_id.to_string(), job_run_id);
        }
        Ok(())
    }

    fn record_event(&self, _event: OrbitEvent) -> Result<(), OrbitError> {
        Ok(())
    }

    fn repo_root(&self) -> Result<String, OrbitError> {
        Ok(self.repo_root.to_string_lossy().to_string())
    }

    fn data_root(&self) -> &Path {
        &self.data_root
    }

    fn list_job_runs_for_gc(&self) -> Result<Vec<JobRun>, OrbitError> {
        Ok(Vec::new())
    }

    fn run_tool_with_context_and_role(
        &self,
        _name: &str,
        _input: Value,
        _role: Role,
        _tool_context: ToolContext,
    ) -> Result<Value, OrbitError> {
        Err(OrbitError::Execution(
            "run_tool_with_context_and_role not needed in setup tests".to_string(),
        ))
    }

    fn maybe_create_failure_task(
        &self,
        _job_id: &str,
        _run_id: &str,
        _error_code: &str,
        _error_message: &str,
        _agent: Option<&str>,
        _model: Option<&str>,
    ) -> Result<(), OrbitError> {
        Ok(())
    }

    fn scoring_enabled(&self) -> bool {
        false
    }

    fn scoreboard_dir(&self) -> &Path {
        &self.scoreboard_dir
    }
}
