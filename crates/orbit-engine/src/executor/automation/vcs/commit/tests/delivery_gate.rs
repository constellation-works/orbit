//! ORB-10313 regression: `commit_batch_changes` must fail closed on the durable
//! execution outcome before it stages files, mutates the index, or creates a
//! commit. An explicit `Outcome: failed` line — like an empty/placeholder
//! summary — leaves HEAD, the index, and the worktree exactly as they were,
//! while other meaningful summaries remain deliverable.
//!
//! ORB-10603 moved read-only checkout resolution and validation ahead of the
//! gate, because the derived summary reads the worktree the gate protects.
//! Nothing that runs before the gate mutates Git state.

use std::fs;
use std::path::Path;

use orbit_types::task::Task;
use serde_json::{Value, json};

use super::super::git_commit;
use super::test_support::*;

use super::super::super::git::{git_output, git_success};
use super::super::super::handoff::reject_failed_delivery;

const GATED_TASK_ID: &str = "ORB-10313-GATE";

fn task_with_summary(summary: &str) -> Task {
    let mut task = task_with_file(
        GATED_TASK_ID,
        "Deliver gated work",
        "src/change.txt",
        "codex",
    );
    task.execution_summary = summary.to_string();
    task
}

fn batch_input(workspace: &Path) -> Value {
    json!({
        "scope": "all",
        "job_run_id": "batch-1",
        "workspace_path": workspace.to_string_lossy().to_string(),
    })
}

/// Stage a fresh repo with an uncommitted change on disk, run the batch commit
/// with the given summary, and assert the delivery gate rejected it without
/// touching HEAD, the index, or the worktree.
fn assert_delivery_blocked(summary: &str, expected_fragment: &str) {
    let temp = initialized_git_repo();
    let workspace = temp.path();
    fs::create_dir_all(workspace.join("src")).unwrap();
    fs::write(workspace.join("src/change.txt"), "would-be delivery\n").unwrap();

    let head_before = git_output(workspace, &["rev-parse", "HEAD"]).expect("HEAD before");
    let status_before = git_output(
        workspace,
        &["status", "--porcelain", "--untracked-files=all"],
    )
    .expect("worktree status before");
    assert_eq!(
        status_before.trim(),
        "?? src/change.txt",
        "precondition: the change is present and unstaged"
    );

    let host = CommitTestHost::new(vec![task_with_summary(summary)], workspace.to_path_buf());
    let error = git_commit(&host, &batch_input(workspace))
        .expect_err("explicit failed outcome must block delivery");
    let message = error.to_string();
    assert!(
        message.contains(GATED_TASK_ID),
        "error names the task: {message}"
    );
    assert!(
        message.contains(expected_fragment),
        "error names the rejected value ({expected_fragment}): {message}"
    );

    assert_eq!(
        git_output(workspace, &["rev-parse", "HEAD"]).expect("HEAD after"),
        head_before,
        "delivery gate must not create a commit"
    );
    assert_eq!(
        git_output(workspace, &["rev-list", "--count", "HEAD"])
            .expect("commit count")
            .trim(),
        "1",
        "only the initial commit remains"
    );
    assert!(
        git_output(workspace, &["diff", "--cached", "--name-only"])
            .expect("staged files after")
            .trim()
            .is_empty(),
        "delivery gate must run before any index staging"
    );
    assert_eq!(
        git_output(
            workspace,
            &["status", "--porcelain", "--untracked-files=all"]
        )
        .expect("worktree status after")
        .trim(),
        "?? src/change.txt",
        "worktree change is left exactly as the implement step produced it"
    );
}

#[test]
fn commit_batch_blocks_failed_outcome_before_any_git_mutation() {
    assert_delivery_blocked(
        "Outcome: failed\n\nChanges:\n- Critical scope unimplemented.",
        "failed",
    );
}

/// ORB-10603: the gate itself is unchanged — empty and placeholder summaries are
/// still refused. What changed is upstream: the commit step now derives a
/// summary from the delivered change first, so this rejection is unreachable in
/// the ordinary case. `summary.rs` covers both halves of that behaviour.
#[test]
fn delivery_gate_still_rejects_empty_and_placeholder_summaries() {
    for summary in ["", "   \n", "TBD", "n/a", "no summary provided"] {
        let error = reject_failed_delivery(&task_with_summary(summary))
            .expect_err("the delivery gate refuses a summary that says nothing");
        let message = error.to_string();
        assert!(
            message.contains(GATED_TASK_ID)
                && message.contains("meaningful persisted execution_summary"),
            "gate names the task and the missing field for '{summary}': {message}"
        );
    }
}

#[test]
fn commit_batch_allows_meaningful_non_failed_outcomes() {
    for summary in [
        "Outcome: success\n\nChanges:\n- Landed the scoped work.",
        "Changes:\n- Did work without a machine-readable outcome.",
        "Outcome: partial\n\nChanges:\n- Reported a non-failure outcome.",
    ] {
        let temp = initialized_git_repo();
        let workspace = temp.path();
        fs::create_dir_all(workspace.join("src")).unwrap();
        fs::write(workspace.join("src/change.txt"), "delivered\n").unwrap();
        git_success(workspace, &["add", "--", "src/change.txt"]).unwrap();

        let host = CommitTestHost::new(vec![task_with_summary(summary)], workspace.to_path_buf());
        let result = git_commit(&host, &batch_input(workspace))
            .expect("a meaningful summary without explicit failure delivers");
        assert_eq!(result["committed"], json!(true));
        assert_eq!(result["task_id"], json!(GATED_TASK_ID));
        assert_eq!(
            git_output(workspace, &["rev-list", "--count", "HEAD"])
                .expect("commit count")
                .trim(),
            "2",
            "the allowed outcome produces exactly one delivery commit"
        );
    }
}
