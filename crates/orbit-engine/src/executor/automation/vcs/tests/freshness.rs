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

/// Build the run-store checkpoint shape a `sync_base` rebase recovery writes,
/// matching `prepared` and the worktree's actual `rewritten_head`. The
/// `workspace_path` must be canonicalized the same way
/// `load_handoff_context` canonicalizes it, or the checkpoint will never be
/// treated as a candidate at all.
fn uncertified_recovery_checkpoint(
    task_id: &str,
    repo: &std::path::Path,
    prepared: &serde_json::Value,
    rewritten_head: &str,
) -> serde_json::Value {
    json!({
        "run_id": "batch-1",
        "step_id": "sync_base",
        "task_ids": [task_id],
        "workspace_path": repo.canonicalize().expect("canonicalize workspace"),
        "head": prepared["head"],
        "head_sha_before": prepared["head_sha"],
        "original_base_sha": prepared["base_sha"],
        "base_ref": prepared["base_ref"],
        "base_sha": prepared["base_sha"],
        "remote_sha_before": prepared["remote_sha"],
        "head_sha": rewritten_head,
        "rewritten": true,
    })
}

fn write_sync_base_checkpoint(host: &PrOpenTestHost, checkpoint: serde_json::Value) {
    let mut state = orbit_types::workflow::PipelineState::new(
        "batch-1".to_string(),
        "task_pr_pipeline".to_string(),
        json!({}),
    );
    state
        .rebase_recovery_checkpoints
        .insert("sync_base".to_string(), checkpoint);
    host.write_run_state(state);
}

// ORB-12015: a `sync_base` rebase recovery checkpoint written before the
// authority boundary (ORB-11977) existed carries no host certificate.
// `verify_rebase_recovery` returns `false` for it exactly as it would for a
// forged one, so it must never be inherited as a trusted HEAD — but the run
// must still be able to make forward progress instead of hard-failing every
// resume forever, per the doc comment on `recovered_head_checkpoint`.
#[test]
fn uncertified_pre_boundary_checkpoint_redoes_the_rebase_instead_of_failing_resume() {
    let workspace = pr_workspace();
    advance_base(&workspace.repo);
    let task_id = "ORB-12015-UNCERTIFIED-REDO";
    let host = PrOpenTestHost::new(
        vec![batch_task(
            task_id,
            "Uncertified redo",
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
    assert_eq!(prepared["sync_required"], json!(true));

    // Simulate a rebase that already completed (e.g. by an older binary,
    // before the authority boundary existed): the branch already sits
    // cleanly on top of the base, but its HEAD no longer matches the
    // prepared pre-rewrite checkpoint.
    git(&workspace.repo, &["rebase", "agent-main"]);
    let rewritten_head = git(&workspace.repo, &["rev-parse", "HEAD"]);
    assert_ne!(rewritten_head, prepared["head_sha"].as_str().unwrap());

    // The row exists in the run store a leaf can write, but nothing ever
    // certified it. That is the pre-boundary shape this task fixes; no
    // manual runtime-store edit is used to unstick it.
    write_sync_base_checkpoint(
        &host,
        uncertified_recovery_checkpoint(task_id, &workspace.repo, &prepared, &rewritten_head),
    );

    let result = rebase_pr_branch(&host, &rebase_input(&common, &prepared))
        .expect("resume must make forward progress through a supported path");
    assert_eq!(result["decision"], json!("performed"));
    assert_eq!(result["rewritten"], json!(true));
    assert!(workspace.repo.join("BASE_ADVANCE.md").exists());

    // Repeating the resume must not loop on the same stale, uncertified
    // entry: the branch is now genuinely fresh, so the next attempt takes
    // the ordinary already-fresh path rather than revisiting recovery.
    let reprepared = prepare_pr_handoff(&host, &common).expect("re-prepare after redo");
    assert_eq!(reprepared["decision"], json!("already_fresh"));
    let retried =
        rebase_pr_branch(&host, &rebase_input(&common, &reprepared)).expect("retry after redo");
    assert_eq!(retried["decision"], json!("skipped_current"));
}

// The redo is only automatic when it is safe: when the discarded rewrite
// actually required conflict resolution, the redo hits the same conflicts
// and falls into the existing supported conflict-recovery path (the same
// typed error a first-time conflicted rebase produces) rather than silently
// dropping work or hard-failing with no path forward.
#[test]
fn uncertified_recovery_redo_that_conflicts_uses_the_existing_conflict_recovery_path() {
    let workspace = rebase_conflict_pr_workspace();
    let task_id = "ORB-12015-UNCERTIFIED-CONFLICT";
    let host = PrOpenTestHost::new(
        vec![batch_task(
            task_id,
            "Uncertified redo with conflict",
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
    assert_eq!(prepared["sync_required"], json!(true));

    // Force the "already rewritten, but uncertified" shape onto a branch
    // whose real rebase conflicts, without actually running the conflicting
    // rebase: fast-forward-merge base into the branch so it looks fresh,
    // then record an (uncertified) recovery checkpoint for that HEAD.
    git(&workspace.repo, &["merge", "-X", "ours", "agent-main"]);
    let rewritten_head = git(&workspace.repo, &["rev-parse", "HEAD"]);
    assert_ne!(rewritten_head, prepared["head_sha"].as_str().unwrap());
    write_sync_base_checkpoint(
        &host,
        uncertified_recovery_checkpoint(task_id, &workspace.repo, &prepared, &rewritten_head),
    );

    let error = rebase_pr_branch(&host, &rebase_input(&common, &prepared))
        .expect_err("redo of a conflicting rebase must not silently succeed or hard-fail");
    assert!(
        matches!(error, OrbitError::RecoverableVcsConflict(_)),
        "unsafe automatic redo must route to the supported conflict-recovery path, got {error}"
    );
    assert!(
        rebase_in_progress(&workspace.repo),
        "conflicted redo must not be aborted; conflict recovery remains retryable"
    );
}

// A checkpoint that *is* certified but whose recorded provenance does not
// match this attempt (a different base, task set, or pre-rewrite HEAD) stays
// a hard refusal: the redo path only ever engages when there is no usable
// certified evidence at all, never to paper over a genuine mismatch.
#[test]
fn certified_but_mismatched_recovery_checkpoint_remains_refused() {
    let workspace = pr_workspace();
    advance_base(&workspace.repo);
    let task_id = "ORB-12015-MISMATCHED-CERT";
    let host = PrOpenTestHost::new(
        vec![batch_task(
            task_id,
            "Mismatched certificate",
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
    git(&workspace.repo, &["rebase", "agent-main"]);
    let rewritten_head = git(&workspace.repo, &["rev-parse", "HEAD"]);

    let mut checkpoint =
        uncertified_recovery_checkpoint(task_id, &workspace.repo, &prepared, &rewritten_head);
    checkpoint["task_ids"] = json!(["ORB-SOME-OTHER-TASK"]);
    write_sync_base_checkpoint(&host, checkpoint.clone());
    host.certify_recovery("batch-1", "sync_base", &checkpoint);

    let error = rebase_pr_branch(&host, &rebase_input(&common, &prepared))
        .expect_err("a certified but mismatched checkpoint must stay refused");
    assert!(
        !matches!(error, OrbitError::RecoverableVcsConflict(_)),
        "a provenance mismatch is not a merge conflict: {error}"
    );
    assert!(
        error
            .to_string()
            .contains("does not match the prepared rewrite checkpoint"),
        "{error}"
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
