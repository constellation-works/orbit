use std::fs;
use std::process::Command;

use serde_json::{Value, json};

use super::super::open::pr_open;
use super::super::promote::pr_promote;
use super::test_support::*;
use crate::context::RuntimeHost;
use crate::executor::automation::vcs::failure::pr_failure_handoff;
use crate::executor::automation::vcs::freshness::{prepare_pr_handoff, rebase_pr_branch};
use crate::executor::automation::vcs::push::push_batch_changes;
use orbit_common::OrbitError;
use orbit_types::task::TaskStatus;

#[test]
fn push_classifies_missing_current_fast_forward_remote_ahead_and_divergent_refs() {
    let missing = pr_workspace();
    git(
        &missing.repo,
        &["push", "origin", "--delete", "orbit/test-batch"],
    );
    let missing_host = PrOpenTestHost::new(Vec::new(), missing.repo.clone());
    assert_eq!(
        push_batch_changes(&missing_host, &generic_push_input(&missing.repo))
            .expect("create missing ref")["decision"],
        json!("performed_create")
    );

    let current = pr_workspace();
    let current_host = PrOpenTestHost::new(Vec::new(), current.repo.clone());
    assert_eq!(
        push_batch_changes(&current_host, &generic_push_input(&current.repo))
            .expect("reuse current ref")["decision"],
        json!("reused_current")
    );
    assert!(current_host.vcs_calls().is_empty());

    let fast_forward = pr_workspace();
    fs::write(fast_forward.repo.join("fast-forward.txt"), "local\n").expect("write local");
    git(&fast_forward.repo, &["add", "fast-forward.txt"]);
    git(&fast_forward.repo, &["commit", "-m", "local follow-up"]);
    let fast_forward_host = PrOpenTestHost::new(Vec::new(), fast_forward.repo.clone());
    assert_eq!(
        push_batch_changes(&fast_forward_host, &generic_push_input(&fast_forward.repo))
            .expect("fast-forward remote")["decision"],
        json!("performed_fast_forward")
    );
    assert_eq!(
        fast_forward_host.vcs_calls()[0].input["force_with_lease"],
        json!(false)
    );

    let remote_ahead = pr_workspace();
    let local_sha = git(&remote_ahead.repo, &["rev-parse", "HEAD"]);
    fs::write(remote_ahead.repo.join("remote-only.txt"), "remote\n").expect("write remote");
    git(&remote_ahead.repo, &["add", "remote-only.txt"]);
    git(&remote_ahead.repo, &["commit", "-m", "remote only"]);
    git(&remote_ahead.repo, &["push", "origin", "orbit/test-batch"]);
    git(&remote_ahead.repo, &["reset", "--hard", &local_sha]);
    let remote_ahead_host = PrOpenTestHost::new(Vec::new(), remote_ahead.repo.clone());
    let error = push_batch_changes(&remote_ahead_host, &generic_push_input(&remote_ahead.repo))
        .expect_err("remote-only commit must not be overwritten");
    assert!(error.to_string().contains("remote-only history"));
    assert!(remote_ahead_host.vcs_calls().is_empty());

    let divergent = pr_workspace();
    let pre_divergence = git(&divergent.repo, &["rev-parse", "HEAD"]);
    fs::write(divergent.repo.join("remote-side.txt"), "remote\n").expect("write remote side");
    git(&divergent.repo, &["add", "remote-side.txt"]);
    git(&divergent.repo, &["commit", "-m", "remote side"]);
    git(&divergent.repo, &["push", "origin", "orbit/test-batch"]);
    let observed_remote = git(&divergent.repo, &["rev-parse", "HEAD"]);
    git(&divergent.repo, &["reset", "--hard", &pre_divergence]);
    fs::write(divergent.repo.join("local-side.txt"), "local\n").expect("write local side");
    git(&divergent.repo, &["add", "local-side.txt"]);
    git(&divergent.repo, &["commit", "-m", "local side"]);
    let divergent_host = PrOpenTestHost::new(Vec::new(), divergent.repo.clone());
    let mut input = generic_push_input(&divergent.repo);
    let error = push_batch_changes(&divergent_host, &input)
        .expect_err("generic divergence cannot authorize force push");
    assert!(error.to_string().contains("no durable rewrite checkpoint"));
    input["rewrite_performed"] = json!(true);
    input["rewrite_head_before"] = json!(pre_divergence);
    input["expected_remote_sha"] = input["rewrite_head_before"].clone();
    let error = push_batch_changes(&divergent_host, &input)
        .expect_err("stale expected remote SHA must not authorize force push");
    assert!(error.to_string().contains("no durable rewrite checkpoint"));
    assert!(divergent_host.vcs_calls().is_empty());
    input["expected_remote_sha"] = json!(observed_remote);
    let result = push_batch_changes(&divergent_host, &input).expect("exact checkpoint authorizes");
    assert_eq!(result["decision"], json!("performed_force_with_lease"));
    let push = divergent_host
        .vcs_calls()
        .into_iter()
        .find(|call| call.operation == PUSH_OPERATION)
        .expect("force push call");
    assert_eq!(push.input["force_with_lease"], json!(true));
    assert_eq!(
        push.input["expected_remote_sha"],
        input["expected_remote_sha"]
    );
}

#[test]
fn push_propagates_private_vcs_failure() {
    let workspace = pr_workspace();
    fs::write(workspace.repo.join("push-failure.txt"), "local\n").expect("write local change");
    git(&workspace.repo, &["add", "push-failure.txt"]);
    git(&workspace.repo, &["commit", "-m", "local push candidate"]);
    let host = PrOpenTestHost::new(Vec::new(), workspace.repo.clone());
    host.fail_vcs(PUSH_OPERATION, "git: simulated push failure");

    let error = push_batch_changes(&host, &generic_push_input(&workspace.repo))
        .expect_err("private push failure must propagate");

    assert!(error.to_string().contains("simulated push failure"));
    assert_eq!(host.vcs_calls().len(), 1);
    assert_eq!(host.vcs_calls()[0].operation, PUSH_OPERATION);
}

#[test]
fn recovered_rebase_continues_remaining_handoff_phases_without_replay() {
    let workspace = rebase_conflict_pr_workspace();
    let task_id = "T20260716-RECOVERY";
    let host = PrOpenTestHost::new(
        vec![batch_task(
            task_id,
            "Recover rebase",
            "Outcome: success\nChanges:\n- Implemented once.",
        )],
        workspace.repo.clone(),
    )
    .with_activity_implementer("codex", "codex");
    let input = json!({
        "workspace_path": workspace.repo,
        "job_run_id": "batch-1",
        "completed_task_ids": [task_id],
        "base": "agent-main",
        "base_sync": "local",
    });
    let commits_before = git(
        &workspace.repo,
        &["rev-list", "--count", "agent-main..orbit/test-batch"],
    );

    let prepared = prepare_pr_handoff(&host, &input).expect("persist preparation checkpoint");
    assert_eq!(prepared["decision"], json!("rebase_required"));
    let rebase_input = rebase_input(&input, &prepared);
    rebase_pr_branch(&host, &rebase_input).expect_err("first rebase conflicts");
    let retry_error =
        rebase_pr_branch(&host, &rebase_input).expect_err("retry reports the still-stopped rebase");
    let retry_message = retry_error.to_string();
    assert!(
        retry_message.contains("rebase remains stopped with unresolved conflicts"),
        "{retry_message}"
    );
    assert!(
        retry_message.contains("conflicting paths: src/lib.rs"),
        "{retry_message}"
    );
    assert!(
        !retry_message.contains("prepared branch"),
        "stopped rebase must not be misclassified as a checkout mismatch: {retry_message}"
    );
    let failure_comment = host
        .comments_for(task_id)
        .into_iter()
        .find(|comment| comment.message.contains("[phase=rebase]"))
        .expect("rebase failure handoff comment");
    assert!(
        failure_comment
            .message
            .contains("Worktree state: rebase stopped with unresolved conflicts."),
        "{}",
        failure_comment.message
    );
    assert!(
        failure_comment.message.contains("`git rebase --continue`"),
        "{}",
        failure_comment.message
    );

    fs::write(
        workspace.repo.join("src/lib.rs"),
        "pub fn diverged() {}\npub fn branch() {}\n",
    )
    .expect("resolve conflict");
    git(&workspace.repo, &["add", "src/lib.rs"]);
    let output = Command::new("git")
        .args(["-c", "core.editor=true", "rebase", "--continue"])
        .current_dir(&workspace.repo)
        .output()
        .expect("continue rebase");
    assert!(
        output.status.success(),
        "rebase continue failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let error = rebase_pr_branch(&host, &rebase_input)
        .expect_err("manual continuation lacks host provenance");
    assert!(
        error
            .to_string()
            .contains("no exact host-validated recovery checkpoint")
    );
    let run_id = rebase_input["job_run_id"].as_str().unwrap();
    crate::context::RuntimeHost::checkpoint_rebase_recovery(
        &host,
        run_id,
        "sync_base",
        &json!({
            "run_id": run_id,
            "step_id": "sync_base",
            "workspace_path": workspace.repo,
            "task_ids": rebase_input["completed_task_ids"],
            "head": rebase_input["head"],
            "head_sha_before": rebase_input["head_sha"],
            "base_ref": rebase_input["base_ref"],
            "base_sha": rebase_input["base_sha"],
            "remote_sha_before": rebase_input["remote_sha"],
            "head_sha": git(&workspace.repo, &["rev-parse", "HEAD"]),
            "rewritten": true,
        }),
    )
    .unwrap();
    let synced = rebase_pr_branch(&host, &rebase_input).expect("reuse recovered rewrite");
    assert_eq!(synced["decision"], json!("reused_recovery"));
    let recovered_head = git(&workspace.repo, &["rev-parse", "HEAD"]);
    git(
        &workspace.repo,
        &["commit", "--allow-empty", "-m", "unattributed replacement"],
    );
    let replacement_error = rebase_pr_branch(&host, &rebase_input).unwrap_err();
    assert!(
        replacement_error
            .to_string()
            .contains("no exact host-validated recovery checkpoint")
    );
    git(&workspace.repo, &["reset", "--hard", &recovered_head]);

    assert_eq!(synced["rewritten"], json!(true));
    assert_eq!(
        git(
            &workspace.repo,
            &[
                "rev-list",
                "--count",
                &format!(
                    "{}..orbit/test-batch",
                    synced["base_sha"].as_str().expect("base sha")
                )
            ]
        ),
        commits_before,
        "recovery must rewrite the existing task commit, not create a second one"
    );

    let push = push_batch_changes(&host, &push_input(&input, &synced)).expect("safe force push");
    assert_eq!(push["decision"], json!("performed_force_with_lease"));

    host.queue_vcs_error(PR_VIEW_OPERATION, "local persistence/view failed");
    let open_input = open_input(&input, &synced);
    pr_open(&host, &open_input).expect_err("first create loses local view result");
    let opened = pr_open(&host, &open_input).expect("retry reuses external PR");
    assert_eq!(opened["decision"], json!("reused"));
    assert_eq!(
        host.vcs_calls()
            .iter()
            .filter(|call| call.operation == PR_CREATE_OPERATION)
            .count(),
        1
    );

    let promoted = pr_promote(
        &host,
        &json!({
            "workspace_path": workspace.repo,
            "job_run_id": "batch-1",
            "completed_task_ids": [task_id],
            "pr_number": opened["pr_number"],
            "pr_url": opened["pr_url"],
        }),
    )
    .expect("promote after reused PR");
    assert_eq!(promoted["decision"], json!("performed"));
    let task = host.get_task(task_id).expect("task");
    assert_eq!(task.status, TaskStatus::Review);
    assert_eq!(task.github_pr_number(), Some("42"));
}

#[test]
fn rebase_retry_distinguishes_wrong_branch_from_stopped_rebase() {
    let workspace = rebase_conflict_pr_workspace();
    let task_id = "ORB-WRONG-BRANCH";
    let host = PrOpenTestHost::new(
        vec![batch_task(
            task_id,
            "Reject wrong branch",
            "Outcome: success\nChanges:\n- Candidate is ready.",
        )],
        workspace.repo.clone(),
    );
    let input = json!({
        "workspace_path": workspace.repo,
        "job_run_id": "batch-1",
        "completed_task_ids": [task_id],
        "base": "agent-main",
        "base_sync": "local",
    });
    let prepared = prepare_pr_handoff(&host, &input).expect("prepare branch checkpoint");
    git(&workspace.repo, &["checkout", "agent-main"]);

    let error = rebase_pr_branch(&host, &rebase_input(&input, &prepared))
        .expect_err("genuine wrong branch remains rejected");
    let message = error.to_string();
    assert!(
        message
            .contains("prepared branch 'orbit/test-batch' is not checked out (found 'agent-main')"),
        "{message}"
    );
    assert!(!message.contains("rebase remains stopped"), "{message}");
}

#[cfg(unix)]
#[test]
fn rebase_disables_tracked_post_rewrite_hook() {
    use std::os::unix::fs::PermissionsExt;

    let workspace = pr_workspace();
    git(&workspace.repo, &["checkout", "agent-main"]);
    fs::create_dir(workspace.repo.join(".hooks")).expect("hooks directory");
    let hook = workspace.repo.join(".hooks/post-rewrite");
    fs::write(
        &hook,
        "#!/bin/sh\nprintf triggered > .git/post-rewrite-marker\n",
    )
    .expect("write rewrite hook");
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).expect("executable hook");
    git(&workspace.repo, &["add", ".hooks"]);
    git(
        &workspace.repo,
        &["commit", "-m", "advance base with tracked hook"],
    );
    git(&workspace.repo, &["checkout", "orbit/test-batch"]);
    git(&workspace.repo, &["config", "core.hooksPath", ".hooks"]);
    let host = PrOpenTestHost::new(
        vec![batch_task("T1", "Rebase candidate", "Outcome: success")],
        workspace.repo.clone(),
    );
    let input = json!({
        "workspace_path": workspace.repo,
        "job_run_id": "batch-1",
        "completed_task_ids": ["T1"],
        "base": "agent-main",
        "base_sync": "local",
    });
    let prepared = prepare_pr_handoff(&host, &input).expect("prepare rebase");
    let synced = rebase_pr_branch(&host, &rebase_input(&input, &prepared)).expect("rebase");
    assert_eq!(synced["rewritten"], true);
    assert!(hook.exists(), "rebased tree includes the executable hook");
    assert!(!workspace.repo.join(".git/post-rewrite-marker").exists());

    // An ordinary amend invokes the exact hook whose rebase execution was suppressed.
    git(&workspace.repo, &["commit", "--amend", "--no-edit"]);
    assert!(workspace.repo.join(".git/post-rewrite-marker").exists());
}

#[test]
fn base_advance_with_mergeable_changes_rebases_cleanly_and_continues() {
    let workspace = pr_workspace();
    git(&workspace.repo, &["checkout", "agent-main"]);
    fs::write(workspace.repo.join("BASE_ADVANCE.md"), "new base\n").expect("write base advance");
    git(&workspace.repo, &["add", "BASE_ADVANCE.md"]);
    git(&workspace.repo, &["commit", "-m", "advance base"]);
    git(&workspace.repo, &["checkout", "orbit/test-batch"]);
    let task_id = "ORB-CLEAN-REBASE";
    let host = PrOpenTestHost::new(
        vec![batch_task(
            task_id,
            "Clean base synchronization",
            "Outcome: success\nChanges:\n- Candidate remains mergeable.",
        )],
        workspace.repo.clone(),
    );
    let input = json!({
        "workspace_path": workspace.repo,
        "job_run_id": "batch-1",
        "completed_task_ids": [task_id],
        "base": "agent-main",
        "base_sync": "local",
    });

    let prepared = prepare_pr_handoff(&host, &input).expect("detect advanced base");
    assert_eq!(prepared["decision"], json!("rebase_required"));
    let synced =
        rebase_pr_branch(&host, &rebase_input(&input, &prepared)).expect("clean rebase succeeds");

    assert_eq!(synced["decision"], json!("performed"));
    assert_eq!(synced["rewritten"], json!(true));
    assert!(
        workspace.repo.join("BASE_ADVANCE.md").exists(),
        "rebased candidate contains the new base commit"
    );
}

#[test]
fn conflicting_rebase_publishes_clean_pre_rebase_branch_and_blocks_task() {
    let workspace = rebase_conflict_pr_workspace();
    let task_id = "ORB-CONFLICT-HANDOFF";
    let host = PrOpenTestHost::new(
        vec![batch_task(
            task_id,
            "Preserve conflicting candidate",
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
    let prepared = prepare_pr_handoff(&host, &common).expect("prepare conflict checkpoint");
    let pre_rebase_sha = git(&workspace.repo, &["rev-parse", "HEAD"]);
    let error = rebase_pr_branch(&host, &rebase_input(&common, &prepared))
        .expect_err("rebase must stop on the fixture conflict");
    let OrbitError::RecoverableVcsConflict(conflict) = &error else {
        panic!("rebase conflict must retain typed recovery evidence: {error}");
    };
    assert_eq!(conflict.operation, "git_rebase");
    assert_eq!(conflict.target_base_sha, prepared["base_sha"]);
    assert_eq!(conflict.conflicting_paths, ["src/lib.rs"]);

    let recovered = pr_failure_handoff(
        &host,
        &json!({
            "failed_step_id": "sync_base",
            "activity_name": "git_rebase",
            "error_code": "pipeline_step_failed",
            "error_message": error.to_string(),
            "run_id": "batch-1",
            "job_input": {
                "task_ids": [task_id],
                "base_branch": "agent-main",
                "base_sync": "local",
            },
            "pipeline": {
                "worktree": {
                    "workspace_path": workspace.repo,
                    "job_run_id": "batch-1",
                    "base_ref": "agent-main",
                },
                "prepare_branch": prepared,
            },
        }),
    )
    .expect("terminal handoff publishes the pre-rebase candidate");

    assert_eq!(recovered["decision"], json!("blocked_conflict_pr"));
    assert_eq!(
        recovered["conflicting_paths"],
        json!(["src/lib.rs"]),
        "diagnostics name only the run's true conflict"
    );
    assert_eq!(
        recovered["target_base_sha"], prepared["base_sha"],
        "the blocked handoff names the exact prepared base that rejected the rebase"
    );
    assert_eq!(git(&workspace.repo, &["rev-parse", "HEAD"]), pre_rebase_sha);
    assert!(
        git(&workspace.repo, &["status", "--porcelain"]).is_empty(),
        "terminal handoff must not leave uncommitted or conflict-marked work"
    );
    let body = host.pr_create_body();
    for expected in [
        "Merge conflict handoff",
        "Original base:",
        "Target base:",
        "`src/lib.rs`",
    ] {
        assert!(
            body.contains(expected),
            "blocked PR body missing {expected:?}: {body}"
        );
    }
    assert!(
        !body.contains("Automatic conflict recovery exhausted"),
        "a conflict alone is not evidence that recovery was attempted"
    );
    let task = host.get_task(task_id).expect("blocked task");
    assert_eq!(task.status, TaskStatus::Blocked);
    assert_eq!(task.github_pr_number(), Some("42"));
    let update = host
        .automation_updates()
        .into_iter()
        .find(|(_, update)| update.status == Some(TaskStatus::Blocked))
        .expect("blocked failure-handoff update");
    assert_eq!(
        update.1.status_event.as_deref(),
        Some("pr_conflict_blocked")
    );
}

#[test]
fn base_advancement_after_prepare_is_preserved_without_starting_stale_recovery() {
    let workspace = rebase_conflict_pr_workspace();
    let task_id = "ORB-BASE-RACE";
    let host = PrOpenTestHost::new(
        vec![batch_task(
            task_id,
            "Preserve concurrent base advancement",
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
    let prepared = prepare_pr_handoff(&host, &common).expect("prepare pinned base");
    git(&workspace.repo, &["checkout", "agent-main"]);
    fs::write(workspace.repo.join("race.txt"), "concurrent base change\n")
        .expect("write concurrent base change");
    git(&workspace.repo, &["add", "race.txt"]);
    git(
        &workspace.repo,
        &["commit", "-m", "advance base during handoff"],
    );
    let advanced_base = git(&workspace.repo, &["rev-parse", "agent-main"]);
    git(&workspace.repo, &["checkout", "orbit/test-batch"]);

    let error = rebase_pr_branch(&host, &rebase_input(&common, &prepared))
        .expect_err("stale prepared base must fail closed");

    assert!(
        matches!(error, OrbitError::Execution(_)),
        "base movement is not an unmerged-index conflict: {error}"
    );
    assert!(error.to_string().contains(&advanced_base), "{error}");
    assert!(
        error
            .to_string()
            .contains("refusing to lose concurrent base changes")
    );
    assert!(
        !workspace.repo.join("race.txt").exists(),
        "candidate branch must not silently absorb or overwrite the newly observed base"
    );
    assert!(
        git(&workspace.repo, &["status", "--porcelain"]).is_empty(),
        "stale checkpoint refusal must not start a rebase"
    );
}

#[test]
fn completion_failure_preserves_published_candidate_and_review_state() {
    let workspace = pr_workspace();
    let task_id = "ORB-COMPLETE-RECOVERY";
    let host = PrOpenTestHost::new(
        vec![review_batch_task(task_id, Some("codex"), None)],
        workspace.repo.clone(),
    );
    let head_before = git(&workspace.repo, &["rev-parse", "HEAD"]);

    let recovered = pr_failure_handoff(
        &host,
        &json!({
            "failed_step_id": "complete_pr",
            "activity_name": "pr_complete",
            "error_code": "pipeline_step_failed",
            "error_message": "GraphQL: Squash merges are not allowed on this repository",
            "run_id": "batch-1",
            "job_input": { "task_ids": [task_id] },
            "pipeline": {},
        }),
    )
    .expect("completion failure recovery");

    assert_eq!(recovered["decision"], "review_completion_failure");
    assert_eq!(recovered["pr_number"], "42");
    assert_eq!(recovered["candidate_preserved"], true);
    assert_eq!(host.task_status(task_id), TaskStatus::Review);
    assert_eq!(git(&workspace.repo, &["rev-parse", "HEAD"]), head_before);
    assert!(
        host.vcs_calls().is_empty(),
        "recovery must not rewrite the PR"
    );
    let task = host.get_task(task_id).expect("task");
    assert_eq!(task.github_pr_number(), Some("42"));
    let comment = host
        .comments_for(task_id)
        .pop()
        .expect("diagnostic comment");
    assert!(comment.message.contains("preserved PR #42"));
    assert!(comment.message.contains("Squash merges are not allowed"));
    assert!(!comment.message.contains("Manual resolution required"));
}

#[test]
fn non_fast_forward_drift_handoff_commits_dirty_work_and_raises_pr() {
    let workspace = no_diff_pr_workspace();
    fs::create_dir_all(workspace.repo.join("src")).expect("create source dir");
    fs::write(
        workspace.repo.join("src/recovered.rs"),
        "pub fn recovered() {}\n",
    )
    .expect("write uncommitted candidate");
    let task_id = "ORB-DIRTY-HANDOFF";
    let host = PrOpenTestHost::new(
        vec![batch_task(
            task_id,
            "Preserve dirty candidate",
            "Outcome: failed\nChanges:\n- Agent stopped after writing code.",
        )],
        workspace.repo.clone(),
    );

    let recovered = pr_failure_handoff(
        &host,
        &json!({
            "failed_step_id": "implement_bundle",
            "activity_name": "agent_implement",
            "error_code": "primary_checkout_drift",
            "error_message": "registered primary moved non-fast-forward after work was persisted",
            "run_id": "batch-1",
            "job_input": {
                "task_ids": [task_id],
                "base_branch": "agent-main",
                "base_sync": "local",
            },
            "pipeline": {
                "worktree": {
                    "workspace_path": workspace.repo,
                    "job_run_id": "batch-1",
                    "base_ref": "agent-main",
                },
            },
        }),
    )
    .expect("dirty work is committed and published");

    assert_eq!(recovered["decision"], json!("blocked_failure_pr"));
    assert_eq!(recovered["committed_files"], json!(["src/recovered.rs"]));
    assert!(
        git(&workspace.repo, &["status", "--porcelain"]).is_empty(),
        "failure handoff must leave a clean worktree"
    );
    assert_eq!(
        git(&workspace.repo, &["log", "-1", "--format=%s"]),
        "chore: Preserve dirty candidate [ORB-DIRTY-HANDOFF]"
    );
    assert!(
        host.vcs_calls()
            .iter()
            .any(|call| call.operation == PUSH_OPERATION),
        "the recovery branch is pushed before PR creation"
    );
    assert!(
        host.pr_create_body().contains("primary_checkout_drift"),
        "the blocked PR preserves the typed primary-drift classification"
    );
    assert!(
        host.pr_create_body().contains("Delivery failure handoff"),
        "non-conflict failures receive the generic handoff"
    );
    assert!(
        !host
            .pr_create_body()
            .contains("Automatic conflict recovery exhausted"),
        "non-conflict failures must not claim an unverified recovery attempt"
    );
    let calls = host.vcs_calls();
    assert!(
        calls
            .iter()
            .position(|call| call.operation == PUSH_OPERATION)
            < calls
                .iter()
                .position(|call| call.operation == PR_CREATE_OPERATION),
        "push precedes PR creation"
    );
}

/// ORB-10313: every resumable PR checkpoint reloads durable state through
/// `load_handoff_context`, so `pr_open` fails closed on an explicit failed
/// execution outcome before any GitHub call, external-ref write, or promotion
/// — and the task stays outside review.
#[test]
fn pr_open_blocks_failed_outcome_before_external_calls() {
    let workspace = pr_workspace();
    let host = PrOpenTestHost::new(
        vec![batch_task(
            "ORB-10313-PR",
            "Gated delivery",
            "Outcome: failed\n\nChanges:\n- Critical scope unimplemented.",
        )],
        workspace.repo.clone(),
    );

    let error = pr_open(&host, &pr_open_input(&workspace.repo, vec!["ORB-10313-PR"]))
        .expect_err("explicit failed outcome must block PR handoff");
    assert!(error.to_string().contains("ORB-10313-PR"), "{error}");
    assert!(error.to_string().contains("failed"), "{error}");
    assert!(host.vcs_calls().is_empty(), "zero GitHub calls");
    assert!(
        host.automation_updates().is_empty(),
        "zero external-ref writes and zero promotion updates"
    );
    let task = host.get_task("ORB-10313-PR").expect("task");
    assert_eq!(
        task.status,
        TaskStatus::InProgress,
        "task remains outside review"
    );
    assert!(task.external_refs.is_empty());
}

#[test]
fn pr_open_allows_meaningful_non_failed_outcomes() {
    for summary in [
        "Changes:\n- No outcome line at all.",
        "Outcome: partial\n\nChanges:\n- Partly done.",
    ] {
        let workspace = pr_workspace();
        let host = PrOpenTestHost::new(
            vec![batch_task("ORB-10313-PR", "Allowed delivery", summary)],
            workspace.repo.clone(),
        );

        pr_open(&host, &pr_open_input(&workspace.repo, vec!["ORB-10313-PR"]))
            .expect("meaningful summary without explicit failure may open a PR");
        assert!(
            host.vcs_calls()
                .iter()
                .any(|call| call.operation == PR_CREATE_OPERATION),
            "the allowed handoff reaches PR creation"
        );
    }
}

/// The same durable gate covers `pr_prepare`, so a resumed prepare revalidates
/// the outcome before it inspects Git or writes any checkpoint.
#[test]
fn pr_prepare_blocks_failed_outcome_before_git_inspection() {
    let workspace = pr_workspace();
    let host = PrOpenTestHost::new(
        vec![batch_task(
            "ORB-10313-PREP",
            "Prepare gated",
            "Outcome: failed\nChanges:\n- unfinished",
        )],
        workspace.repo.clone(),
    );
    let input = json!({
        "workspace_path": workspace.repo,
        "job_run_id": "batch-1",
        "completed_task_ids": ["ORB-10313-PREP"],
        "base": "agent-main",
        "base_sync": "local",
    });

    let error =
        prepare_pr_handoff(&host, &input).expect_err("prepare must revalidate durable outcome");
    assert!(error.to_string().contains("ORB-10313-PREP"), "{error}");
    assert!(error.to_string().contains("failed"), "{error}");
    assert!(host.vcs_calls().is_empty());
    assert!(host.automation_updates().is_empty());
}

/// The no-diff promotion path also runs through `load_handoff_context`, so a
/// failed outcome cannot bypass the gate even when the task carries the
/// no-diff-expected tag.
#[test]
fn pr_promote_no_diff_blocks_failed_outcome() {
    let workspace = no_diff_pr_workspace();
    let mut task = batch_task(
        "ORB-10313-ND",
        "No-diff gated",
        "Outcome: failed\nChanges:\n- Nothing safe to promote.",
    );
    task.tags
        .push(orbit_types::task::NO_DIFF_EXPECTED_TAG.to_string());
    let host = PrOpenTestHost::new(vec![task], workspace.repo.clone())
        .with_activity_implementer("codex", "codex");

    let error = pr_promote(
        &host,
        &json!({
            "workspace_path": workspace.repo,
            "job_run_id": "batch-1",
            "completed_task_ids": ["ORB-10313-ND"],
            "no_diff_expected": true,
        }),
    )
    .expect_err("failed outcome must block no-diff promotion");
    assert!(error.to_string().contains("ORB-10313-ND"), "{error}");
    assert!(error.to_string().contains("failed"), "{error}");
    assert!(host.vcs_calls().is_empty());
    assert!(host.automation_updates().is_empty());
    let task = host.get_task("ORB-10313-ND").expect("task");
    assert_eq!(task.status, TaskStatus::InProgress);
    assert!(task.external_refs.is_empty());
}

fn generic_push_input(repo: &std::path::Path) -> Value {
    json!({
        "workspace_path": repo,
        "branch": "orbit/test-batch",
    })
}

fn rebase_input(common: &Value, prepared: &Value) -> Value {
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
    })
}

fn push_input(common: &Value, synced: &Value) -> Value {
    json!({
        "workspace_path": common["workspace_path"],
        "job_run_id": common["job_run_id"],
        "completed_task_ids": common["completed_task_ids"],
        "branch": synced["head"],
        "rewrite_performed": synced["rewritten"],
        "rewrite_head_before": synced["head_sha_before"],
        "expected_remote_sha": synced["remote_sha_before"],
    })
}

fn open_input(common: &Value, synced: &Value) -> Value {
    json!({
        "workspace_path": common["workspace_path"],
        "job_run_id": common["job_run_id"],
        "completed_task_ids": common["completed_task_ids"],
        "head": synced["head"],
        "base": synced["base"],
        "base_ref": synced["base_ref"],
        "base_sha": synced["base_sha"],
    })
}
