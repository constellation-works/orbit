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

/// What a previous claimed attempt's failure settlement leaves on the owner.
const PREVIOUS_ATTEMPT_FAILED: &str =
    "Outcome: failed\nClaimed leaf run jrun-previous terminated as failed at step 'commit'.";

/// A batch input carrying this run's implementer output, as both claimed
/// pipelines pass it.
fn claimed_input(workspace: &Path, implementer_summary: &str) -> Value {
    let mut input = batch_input(workspace);
    input["implementation"] = json!({
        "summary": "implemented the claimed change",
        "execution_summary": implementer_summary,
    });
    input
}

/// Run the batch commit over an uncommitted change and assert it was refused
/// without touching HEAD, the index, or the worktree.
fn assert_refused_before_git_mutation(
    host: impl FnOnce(&Path) -> CommitTestHost,
    input: impl FnOnce(&Path) -> Value,
    expected_fragment: &str,
) {
    let temp = initialized_git_repo();
    let workspace = temp.path();
    fs::create_dir_all(workspace.join("src")).unwrap();
    fs::write(workspace.join("src/change.txt"), "would-be delivery\n").unwrap();
    let head_before = git_output(workspace, &["rev-parse", "HEAD"]).expect("HEAD before");

    let host = host(workspace);
    let error = git_commit(&host, &input(workspace)).expect_err("delivery must be refused");
    let message = error.to_string();
    assert!(
        message.contains(GATED_TASK_ID) && message.contains(expected_fragment),
        "error names the task and the refusal ({expected_fragment}): {message}"
    );
    assert_eq!(
        git_output(workspace, &["rev-parse", "HEAD"]).expect("HEAD after"),
        head_before,
        "a refused delivery creates no commit"
    );
    assert!(
        git_output(workspace, &["diff", "--cached", "--name-only"])
            .expect("staged files after")
            .trim()
            .is_empty(),
        "a refused delivery stages nothing"
    );
    assert_eq!(
        git_output(
            workspace,
            &["status", "--porcelain", "--untracked-files=all"]
        )
        .expect("worktree status after")
        .trim(),
        "?? src/change.txt",
        "the implement step's change is left as it was"
    );
    assert!(
        host.persisted_summaries().is_empty(),
        "a refused delivery writes no summary"
    );
}

/// ORB-13755: implementer output in the input is no override for a local run.
/// Without the claim's worker binding the durable summary is the delivery
/// source of truth, exactly as ORB-10313 requires.
#[test]
fn a_local_run_keeps_the_durable_gate_whatever_its_input_reports() {
    assert_refused_before_git_mutation(
        |workspace| {
            CommitTestHost::new(
                vec![task_with_summary(PREVIOUS_ATTEMPT_FAILED)],
                workspace.to_path_buf(),
            )
        },
        |workspace| claimed_input(workspace, "Outcome: done\nChanges:\n- the work"),
        "persisted execution_summary begins with 'Outcome: failed'",
    );
}

/// ORB-13755: a claimed retry delivers on its own implementer's summary. The
/// owner's stored `Outcome: failed` belongs to the previous attempt, and the
/// commit step writes nothing over it: the handoff acceptance does that.
#[test]
fn a_claimed_attempt_delivers_past_a_previous_attempts_failed_summary() {
    let temp = initialized_git_repo();
    let workspace = temp.path();
    fs::create_dir_all(workspace.join("src")).unwrap();
    fs::write(workspace.join("src/change.txt"), "delivered\n").unwrap();

    let host = CommitTestHost::new(
        vec![task_with_summary(PREVIOUS_ATTEMPT_FAILED)],
        workspace.to_path_buf(),
    )
    .with_claim_binding(GATED_TASK_ID);
    let result = git_commit(
        &host,
        &claimed_input(workspace, "Outcome: done\nChanges:\n- the work"),
    )
    .expect("this attempt reports done, so its candidate is committed");
    assert_eq!(result["committed"], json!(true));
    assert_eq!(
        git_output(workspace, &["rev-list", "--count", "HEAD"])
            .expect("commit count")
            .trim(),
        "2",
        "exactly one delivery commit"
    );
    assert!(
        host.persisted_summaries().is_empty(),
        "a claimed commit writes no owner summary"
    );
    assert_eq!(
        host.task_execution_summary(GATED_TASK_ID),
        PREVIOUS_ATTEMPT_FAILED
    );
}

/// ORB-13755: the claimed gate still fails closed on a current failure, even
/// over a stored summary that would have let the work through.
#[test]
fn a_claimed_attempt_reporting_failure_is_refused_before_git_mutation() {
    assert_refused_before_git_mutation(
        |workspace| {
            CommitTestHost::new(
                vec![task_with_summary("Outcome: success\nChanges:\n- earlier")],
                workspace.to_path_buf(),
            )
            .with_claim_binding(GATED_TASK_ID)
        },
        |workspace| claimed_input(workspace, "Outcome: failed\nChanges:\n- gave up"),
        "this claimed attempt's implementer summary begins with 'Outcome: failed'",
    );
}

/// ORB-13755: a claimed step handed no implementer output has nothing of this
/// attempt to judge, so it falls back to the durable gate rather than open.
#[test]
fn a_claimed_step_without_implementer_output_keeps_the_durable_gate() {
    assert_refused_before_git_mutation(
        |workspace| {
            CommitTestHost::new(
                vec![task_with_summary(PREVIOUS_ATTEMPT_FAILED)],
                workspace.to_path_buf(),
            )
            .with_claim_binding(GATED_TASK_ID)
        },
        batch_input,
        "persisted execution_summary begins with 'Outcome: failed'",
    );
}

/// ORB-13755: the binding scopes the claimed gate to the claim's own task.
#[test]
fn a_claim_bound_to_another_task_keeps_the_durable_gate() {
    assert_refused_before_git_mutation(
        |workspace| {
            CommitTestHost::new(
                vec![task_with_summary(PREVIOUS_ATTEMPT_FAILED)],
                workspace.to_path_buf(),
            )
            .with_claim_binding("ORB-OTHER")
        },
        |workspace| claimed_input(workspace, "Outcome: done\nChanges:\n- the work"),
        "persisted execution_summary begins with 'Outcome: failed'",
    );
}
