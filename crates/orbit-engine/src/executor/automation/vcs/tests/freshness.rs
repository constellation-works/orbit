#![allow(missing_docs)]

use std::fs;
use std::process::Command;

use serde_json::json;
use tempfile::tempdir;

use super::super::freshness::{prepare_pr_handoff, rebase_pr_branch};
use super::super::pr::tests::test_support::{
    PrOpenTestHost, batch_task, pr_workspace, rebase_conflict_pr_workspace,
};
#[cfg(unix)]
use super::with_fake_git;
use orbit_common::OrbitError;

fn git(current_dir: &std::path::Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(current_dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn rebase_in_progress(repo: &std::path::Path) -> bool {
    repo.join(".git/rebase-merge").is_dir() || repo.join(".git/rebase-apply").is_dir()
}

fn rebase_input(common: &serde_json::Value, prepared: &serde_json::Value) -> serde_json::Value {
    json!({
        "workspace_path": common["workspace_path"],
        "job_run_id": common["job_run_id"],
        "completed_task_ids": common["completed_task_ids"],
        "head": prepared["head"],
        "head_sha": prepared["head_sha"],
        "base": prepared["base"],
        "base_ref": prepared["base_ref"],
        "base_sha": prepared["base_sha"],
        "remote_sha": prepared["remote_sha"],
        "commits_behind": prepared["commits_behind"],
        "sync_required": prepared["sync_required"],
        "git_timeouts": { "rebase": 400 },
    })
}

fn advance_base(workspace_repo: &std::path::Path) {
    git(workspace_repo, &["checkout", "agent-main"]);
    fs::write(workspace_repo.join("BASE_ADVANCE.md"), "new base\n").unwrap();
    git(workspace_repo, &["add", "BASE_ADVANCE.md"]);
    git(workspace_repo, &["commit", "-m", "advance base"]);
    git(workspace_repo, &["checkout", "orbit/test-batch"]);
}

#[cfg(unix)]
#[test]
fn owned_rebase_timeout_aborts_and_retry_is_not_permanently_wedged() {
    let stamp_dir = tempdir().unwrap();
    let stamp = stamp_dir.path().join("rebase-once");
    if !with_fake_git(
        module_path!(),
        "owned_rebase_timeout_aborts_and_retry_is_not_permanently_wedged",
        &[("ORBIT_TEST_GIT_REBASE_ONCE", stamp.display().to_string())],
    ) {
        return;
    }

    let workspace = pr_workspace();
    advance_base(&workspace.repo);
    let task_id = "ORB-11606-REBASE-TIMEOUT";
    let host = PrOpenTestHost::new(
        vec![batch_task(
            task_id,
            "Timeout rebase",
            "Outcome: success\nChanges:\n- Candidate remains mergeable.",
        )],
        workspace.repo.clone(),
    );
    let common = json!({
        "workspace_path": workspace.repo,
        "job_run_id": "batch-1",
        "completed_task_ids": [task_id],
        "base": "agent-main",
        "base_sync": "local",
    });
    let prepared = prepare_pr_handoff(&host, &common).expect("prepare");
    let first = rebase_pr_branch(&host, &rebase_input(&common, &prepared))
        .expect_err("injected rebase timeout");
    let first_message = first.to_string();
    assert!(
        first_message.contains("timed out"),
        "expected timeout, got {first_message}"
    );
    assert!(
        first_message.contains("timeout recovery") || first_message.contains("Timeout recovery"),
        "must distinguish timeout recovery: {first_message}"
    );
    assert!(
        !matches!(first, OrbitError::RecoverableVcsConflict(_)),
        "timeout recovery must not be a conflict: {first}"
    );
    assert!(
        !rebase_in_progress(&workspace.repo),
        "owned timed-out rebase must be aborted so retry is not wedged"
    );

    let retried = rebase_pr_branch(&host, &rebase_input(&common, &prepared)).expect("retry");
    assert_eq!(retried["decision"], json!("performed"));
    assert!(workspace.repo.join("BASE_ADVANCE.md").exists());
}

#[cfg(unix)]
#[test]
fn rebase_stderr_timeout_phrase_is_ordinary_failure_not_timeout_recovery() {
    if !with_fake_git(
        module_path!(),
        "rebase_stderr_timeout_phrase_is_ordinary_failure_not_timeout_recovery",
        &[("ORBIT_TEST_GIT_PHRASE_FAIL", "rebase".to_string())],
    ) {
        return;
    }

    let workspace = pr_workspace();
    advance_base(&workspace.repo);
    let task_id = "ORB-11802-REBASE-PHRASE";
    let host = PrOpenTestHost::new(
        vec![batch_task(
            task_id,
            "Phrase rebase",
            "Outcome: success\nChanges:\n- Candidate remains mergeable.",
        )],
        workspace.repo.clone(),
    );
    let common = json!({
        "workspace_path": workspace.repo,
        "job_run_id": "batch-1",
        "completed_task_ids": [task_id],
        "base": "agent-main",
        "base_sync": "local",
    });
    let prepared = prepare_pr_handoff(&host, &common).expect("prepare");
    let error = rebase_pr_branch(&host, &rebase_input(&common, &prepared))
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
        !matches!(error, OrbitError::RecoverableVcsConflict(_)),
        "phrase failure is not a conflict: {error}"
    );
    assert!(
        !rebase_in_progress(&workspace.repo),
        "ordinary rebase failure must not leave timeout-aborted rebase state"
    );
}

#[test]
fn pre_existing_rebase_without_conflicts_is_refused_and_left_intact() {
    let workspace = pr_workspace();
    advance_base(&workspace.repo);
    let head_before = git(&workspace.repo, &["rev-parse", "HEAD"]);
    let onto = git(&workspace.repo, &["rev-parse", "agent-main"]);
    fs::create_dir_all(workspace.repo.join(".git/rebase-merge")).unwrap();
    fs::write(
        workspace.repo.join(".git/rebase-merge/orig-head"),
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n",
    )
    .unwrap();
    fs::write(
        workspace.repo.join(".git/rebase-merge/onto"),
        format!("{onto}\n"),
    )
    .unwrap();
    fs::write(
        workspace.repo.join(".git/rebase-merge/head-name"),
        "refs/heads/foreign-branch\n",
    )
    .unwrap();
    fs::write(workspace.repo.join(".git/rebase-merge/git-rebase-todo"), "").unwrap();
    fs::write(workspace.repo.join(".git/rebase-merge/end"), "1\n").unwrap();
    fs::write(workspace.repo.join(".git/rebase-merge/msgnum"), "1\n").unwrap();
    fs::write(workspace.repo.join("retained.txt"), "keep me\n").unwrap();

    let task_id = "ORB-11606-FOREIGN-REBASE";
    let host = PrOpenTestHost::new(
        vec![batch_task(
            task_id,
            "Foreign rebase",
            "Outcome: success\nChanges:\n- Candidate remains mergeable.",
        )],
        workspace.repo.clone(),
    );
    let common = json!({
        "workspace_path": workspace.repo,
        "job_run_id": "batch-1",
        "completed_task_ids": [task_id],
        "base": "agent-main",
        "base_sync": "local",
    });
    let prepared = prepare_pr_handoff(&host, &common).expect("prepare");
    let error = rebase_pr_branch(&host, &rebase_input(&common, &prepared))
        .expect_err("foreign rebase must be refused");
    let message = error.to_string();
    assert!(
        message.contains("pre-existing rebase"),
        "expected diagnostic refusal, got {message}"
    );
    assert!(
        !matches!(error, OrbitError::RecoverableVcsConflict(_)),
        "foreign rebase is not a conflict: {error}"
    );
    assert!(
        rebase_in_progress(&workspace.repo),
        "foreign rebase state must remain"
    );
    assert_eq!(git(&workspace.repo, &["rev-parse", "HEAD"]), head_before);
    assert_eq!(
        fs::read_to_string(workspace.repo.join("retained.txt")).unwrap(),
        "keep me\n"
    );
}

#[test]
fn conflicting_rebase_still_uses_conflict_recovery_not_timeout_abort() {
    let workspace = rebase_conflict_pr_workspace();
    let task_id = "ORB-11606-CONFLICT";
    let host = PrOpenTestHost::new(
        vec![batch_task(
            task_id,
            "Conflict rebase",
            "Outcome: success\nChanges:\n- Candidate is complete.",
        )],
        workspace.repo.clone(),
    );
    let common = json!({
        "workspace_path": workspace.repo,
        "job_run_id": "batch-1",
        "completed_task_ids": [task_id],
        "base": "agent-main",
        "base_sync": "local",
    });
    let prepared = prepare_pr_handoff(&host, &common).expect("prepare");
    let error =
        rebase_pr_branch(&host, &rebase_input(&common, &prepared)).expect_err("fixture conflict");
    assert!(
        matches!(error, OrbitError::RecoverableVcsConflict(_)),
        "conflicts stay typed recovery, got {error}"
    );
    assert!(
        rebase_in_progress(&workspace.repo),
        "conflicted rebase must not be aborted as a timeout"
    );
}
