//! Completion-authorized delivery [ORB-11187].
//!
//! Every case drives the real `pr_complete` / `task_complete` code against the
//! shared fake host with a scripted sequence of `pr.status` answers, so the
//! merge-state machine is exercised without GitHub and without a wall clock:
//! `poll_interval_seconds: 0` makes the poll loop iterate immediately.

use orbit_types::task::{NO_DIFF_EXPECTED_TAG, TaskStatus};
use serde_json::{Value, json};
use tempfile::tempdir;

use super::super::super::super::task_update::task_complete;
use super::super::complete::pr_complete;
use super::test_support::{
    PR_MERGE_CAPABILITIES_OPERATION, PR_MERGE_OPERATION, PR_STATUS_OPERATION, PrOpenTestHost,
    review_batch_task,
};

fn host(tasks: Vec<orbit_types::task::Task>) -> (tempfile::TempDir, PrOpenTestHost) {
    let root = tempdir().expect("create tempdir");
    let repo_root = root.path().to_path_buf();
    let host = PrOpenTestHost::new(tasks, repo_root);
    (root, host)
}

/// Without repository auto-merge, settling checks are polled until GitHub
/// reports a normal mergeable state, then the permitted ordinary merge is used.
#[test]
fn pending_checks_with_auto_merge_disabled_wait_for_an_ordinary_merge() {
    let (root, host) = host(vec![review_batch_task("T1", None, None)]);
    host.queue_pr_status([state("PENDING"), state("CLEAN"), merged_state()]);
    host.queue_merge_capabilities_with_auto_merge(true, true, true, true, false);

    let mut input = complete_input(root.path(), &["T1"]);
    input["max_wait_seconds"] = json!(1);
    let output = pr_complete(&host, &input).expect("complete after checks settle");

    assert_eq!(output["merge"]["merged"], true);
    assert_eq!(output["merge"]["auto_merge_requested"], false);
    let merges = merge_calls(&host);
    assert_eq!(merges.len(), 1, "only the ordinary merge is requested");
    assert_eq!(merges[0]["auto"], false);
    assert_eq!(merges[0]["strategy"], "squash");
    assert_eq!(host.task_status("T1"), TaskStatus::Done);
}

#[test]
fn pending_checks_with_auto_merge_disabled_time_out_in_review() {
    let (root, host) = host(vec![review_batch_task("T1", None, None)]);
    host.queue_pr_status([state("PENDING")]);
    host.queue_merge_capabilities_with_auto_merge(true, true, true, true, false);

    let error = pr_complete(&host, &complete_input(root.path(), &["T1"]))
        .expect_err("pending checks must respect the completion deadline");

    assert!(error.to_string().contains("timed out"), "{error}");
    assert!(
        merge_calls(&host).is_empty(),
        "disabled auto-merge is never requested"
    );
    assert_eq!(host.task_status("T1"), TaskStatus::Review);
}

fn complete_input(workspace_path: &std::path::Path, task_ids: &[&str]) -> Value {
    json!({
        "job_run_id": "batch-1",
        "completed_task_ids": task_ids,
        "workspace_path": workspace_path.to_string_lossy(),
        "pr_number": "42",
        "poll_interval_seconds": 0,
        "max_wait_seconds": 0,
    })
}

fn merged_state() -> Value {
    json!({ "number": 42, "state": "MERGED", "mergedAt": "2026-09-05T00:00:00Z" })
}

fn state(merge_state_status: &str) -> Value {
    json!({ "number": 42, "state": "OPEN", "mergedAt": Value::Null, "mergeStateStatus": merge_state_status })
}

fn merge_calls(host: &PrOpenTestHost) -> Vec<Value> {
    host.vcs_calls()
        .into_iter()
        .filter(|call| call.operation == PR_MERGE_OPERATION)
        .map(|call| call.input)
        .collect()
}

/// A PR that is already merged needs no merge request at all — completion just
/// verifies and transitions.
#[test]
fn an_already_merged_pr_completes_without_requesting_a_merge() {
    let (root, host) = host(vec![review_batch_task("T1", None, None)]);
    host.queue_pr_status([merged_state()]);

    let output = pr_complete(&host, &complete_input(root.path(), &["T1"])).expect("complete");

    assert_eq!(output["merge"]["merged"], true);
    assert_eq!(output["completed_task_ids"], json!(["T1"]));
    assert_eq!(host.task_status("T1"), TaskStatus::Done);
    assert!(
        merge_calls(&host).is_empty(),
        "an already-merged PR must not be merged again"
    );
}

/// The ordinary green path: mergeable now, merged on the next read.
#[test]
fn a_mergeable_pr_is_merged_and_then_verified_before_completing() {
    let (root, host) = host(vec![review_batch_task("T1", None, None)]);
    host.queue_pr_status([state("CLEAN"), merged_state()]);

    let output = pr_complete(&host, &complete_input(root.path(), &["T1"])).expect("complete");

    assert_eq!(output["merge"]["merged"], true);
    assert_eq!(output["merge"]["auto_merge_requested"], false);
    let merges = merge_calls(&host);
    assert_eq!(merges.len(), 1, "exactly one merge request");
    assert_eq!(merges[0]["auto"], false);
    assert_eq!(merges[0]["strategy"], "squash");
    assert_eq!(output["merge"]["strategy"], "squash");
    assert_eq!(host.task_status("T1"), TaskStatus::Done);
}

/// Pending required checks hand the merge to GitHub auto-merge — which respects
/// those checks — and completion still waits for the merged state.
#[test]
fn pending_checks_use_auto_merge_and_completion_waits_for_the_merged_state() {
    let (root, host) = host(vec![review_batch_task("T1", None, None)]);
    host.queue_pr_status([state("PENDING"), state("PENDING"), merged_state()]);

    let mut input = complete_input(root.path(), &["T1"]);
    input["max_wait_seconds"] = json!(600);
    let output = pr_complete(&host, &input).expect("complete");

    assert_eq!(output["merge"]["merged"], true);
    assert_eq!(output["merge"]["auto_merge_requested"], true);
    let merges = merge_calls(&host);
    assert_eq!(
        merges.len(),
        1,
        "auto-merge is requested once, not per poll"
    );
    assert_eq!(merges[0]["auto"], true);
    assert_eq!(merges[0]["strategy"], "squash");
    assert!(
        merges[0].get("admin").is_none(),
        "completion must never request an administrative bypass"
    );
    assert_eq!(host.task_status("T1"), TaskStatus::Done);
}

/// The live incident shape: squash is disabled while rebase and merge commits
/// are enabled. Linear history keeps merge commits out, so both immediate and
/// auto merge must select rebase and retain that method in completion evidence.
#[test]
fn squash_disabled_repository_uses_rebase_for_direct_and_auto_merge() {
    for (initial, auto) in [("CLEAN", false), ("PENDING", true)] {
        let (root, host) = host(vec![review_batch_task("T1", None, None)]);
        host.queue_pr_status([state(initial), merged_state()]);
        host.queue_merge_capabilities(false, true, true, true);
        let mut input = complete_input(root.path(), &["T1"]);
        input["max_wait_seconds"] = json!(1);

        let output = pr_complete(&host, &input).expect("rebase completion");

        let merges = merge_calls(&host);
        assert_eq!(merges.len(), 1);
        assert_eq!(merges[0]["strategy"], "rebase");
        assert_eq!(merges[0]["auto"], auto);
        assert_eq!(output["merge"]["strategy"], "rebase");
        assert_eq!(host.task_status("T1"), TaskStatus::Done);
    }
}

#[test]
fn merge_only_repository_uses_merge_commit_when_linear_history_allows_it() {
    let (root, host) = host(vec![review_batch_task("T1", None, None)]);
    host.queue_pr_status([state("CLEAN"), merged_state()]);
    host.queue_merge_capabilities(false, false, true, false);

    let output = pr_complete(&host, &complete_input(root.path(), &["T1"]))
        .expect("merge-only repository completion");

    assert_eq!(merge_calls(&host)[0]["strategy"], "merge");
    assert_eq!(output["merge"]["strategy"], "merge");
}

#[test]
fn no_policy_permitted_merge_method_fails_without_request_or_bypass() {
    let (root, host) = host(vec![review_batch_task("T1", None, None)]);
    host.queue_pr_status([state("CLEAN")]);
    host.queue_merge_capabilities(false, false, true, true);

    let error = pr_complete(&host, &complete_input(root.path(), &["T1"]))
        .expect_err("linear-history merge-only repository has no permitted method");

    let message = error.to_string();
    assert!(message.contains("no permitted merge method"), "{message}");
    assert!(
        message.contains("requires_linear_history=true"),
        "{message}"
    );
    assert!(merge_calls(&host).is_empty());
    assert_eq!(host.task_status("T1"), TaskStatus::Review);
}

#[test]
fn repository_capability_api_failure_is_explicit_and_precedes_merge() {
    let (root, host) = host(vec![review_batch_task("T1", None, None)]);
    host.queue_pr_status([state("CLEAN")]);
    host.fail_vcs(
        PR_MERGE_CAPABILITIES_OPERATION,
        "gh: Resource not accessible by integration",
    );

    let error = pr_complete(&host, &complete_input(root.path(), &["T1"]))
        .expect_err("capability permission failure must propagate");

    let message = error.to_string();
    assert!(message.contains("could not resolve a permitted merge method"));
    assert!(message.contains("Resource not accessible by integration"));
    assert!(merge_calls(&host).is_empty());
    assert_eq!(host.task_status("T1"), TaskStatus::Review);
}

#[test]
fn direct_merge_api_failure_names_the_selected_method_and_keeps_review() {
    let (root, host) = host(vec![review_batch_task("T1", None, None)]);
    host.queue_pr_status([state("CLEAN")]);
    host.queue_merge_capabilities(false, true, true, true);
    host.fail_vcs(PR_MERGE_OPERATION, "gh: merge permission denied");

    let error = pr_complete(&host, &complete_input(root.path(), &["T1"]))
        .expect_err("merge permission failure must propagate");

    let message = error.to_string();
    assert!(
        message.contains("could not request rebase merge"),
        "{message}"
    );
    assert!(message.contains("merge permission denied"), "{message}");
    assert_eq!(host.task_status("T1"), TaskStatus::Review);
}

/// Enabling auto-merge is not terminal success: if the PR never reaches the
/// merged state within the budget, the run fails and the task stays in review.
#[test]
fn enabling_auto_merge_alone_never_completes_the_task() {
    let (root, host) = host(vec![review_batch_task("T1", None, None)]);
    host.queue_pr_status([state("PENDING"), state("PENDING")]);

    let error = pr_complete(&host, &complete_input(root.path(), &["T1"]))
        .expect_err("a PR that never merges must fail the run");

    assert!(
        error.to_string().contains("timed out"),
        "unexpected error: {error}"
    );
    assert_eq!(
        host.task_status("T1"),
        TaskStatus::Review,
        "a timed-out merge must leave the task in review"
    );
}

/// A PR GitHub refuses to merge fails the run; the guard is never bypassed.
#[test]
fn protected_branch_states_fail_the_run_with_the_task_left_in_review() {
    for (merge_state, expected) in [
        ("BLOCKED", "branch protection or required reviews"),
        ("DIRTY", "merge conflicts"),
        ("BEHIND", "behind its base"),
        ("DRAFT", "still a draft"),
    ] {
        let (root, host) = host(vec![review_batch_task("T1", None, None)]);
        host.queue_pr_status([state(merge_state)]);

        let error = pr_complete(&host, &complete_input(root.path(), &["T1"]))
            .err()
            .unwrap_or_else(|| panic!("{merge_state} must fail the run"));

        assert!(
            error.to_string().contains(expected),
            "{merge_state}: unexpected error: {error}"
        );
        assert_eq!(host.task_status("T1"), TaskStatus::Review);
        assert!(
            merge_calls(&host).is_empty(),
            "{merge_state} must not be forced through"
        );
    }
}

/// A closed-without-merge PR is an actionable failure, not a silent success.
#[test]
fn a_closed_pr_fails_the_run_with_the_task_left_in_review() {
    let (root, host) = host(vec![review_batch_task("T1", None, None)]);
    host.queue_pr_status([json!({ "number": 42, "state": "CLOSED", "mergedAt": Value::Null })]);

    let error = pr_complete(&host, &complete_input(root.path(), &["T1"]))
        .expect_err("a closed PR must fail the run");

    assert!(
        error.to_string().contains("closed without being merged"),
        "unexpected error: {error}"
    );
    assert_eq!(host.task_status("T1"), TaskStatus::Review);
}

/// A refused auto-merge request surfaces as an actionable failure.
#[test]
fn a_refused_auto_merge_request_fails_the_run() {
    let (root, host) = host(vec![review_batch_task("T1", None, None)]);
    host.queue_pr_status([state("PENDING")]);
    host.queue_vcs_error(
        PR_MERGE_OPERATION,
        "auto-merge is not enabled for this repository",
    );

    let error = pr_complete(&host, &complete_input(root.path(), &["T1"]))
        .expect_err("a refused auto-merge must fail the run");

    assert!(
        error.to_string().contains("could not enable auto-merge"),
        "unexpected error: {error}"
    );
    assert_eq!(host.task_status("T1"), TaskStatus::Review);
}

/// Verification failure (the status read itself) is not treated as merged.
#[test]
fn an_unreadable_merge_state_fails_rather_than_assuming_merged() {
    let (root, host) = host(vec![review_batch_task("T1", None, None)]);
    host.fail_vcs(PR_STATUS_OPERATION, "gh: API rate limit exceeded");

    let error = pr_complete(&host, &complete_input(root.path(), &["T1"]))
        .expect_err("an unreadable merge state must fail the run");

    assert!(
        error.to_string().contains("rate limit"),
        "unexpected: {error}"
    );
    assert_eq!(host.task_status("T1"), TaskStatus::Review);
}

/// [AC7] Validated no-diff work completes without a PR to merge.
#[test]
fn no_diff_expected_work_completes_without_a_pull_request() {
    let mut task = review_batch_task("T1", None, None);
    task.tags = vec![NO_DIFF_EXPECTED_TAG.to_string()];
    task.external_refs.clear();
    let (root, host) = host(vec![task]);

    let output = pr_complete(
        &host,
        &json!({
            "job_run_id": "batch-1",
            "completed_task_ids": ["T1"],
            "workspace_path": root.path().to_string_lossy(),
            "no_diff_expected": true,
        }),
    )
    .expect("no-diff completion");

    assert_eq!(output["no_diff_expected"], true);
    assert_eq!(host.task_status("T1"), TaskStatus::Done);
    assert!(
        host.vcs_calls().is_empty(),
        "no-diff completion must not talk to GitHub at all"
    );
}

/// A bundle claiming no-diff without the tag is refused, exactly as promotion
/// refuses it — completion authority does not relax the tag contract.
#[test]
fn no_diff_completion_requires_every_task_to_carry_the_tag() {
    let (root, host) = host(vec![review_batch_task("T1", None, None)]);

    let error = pr_complete(
        &host,
        &json!({
            "job_run_id": "batch-1",
            "completed_task_ids": ["T1"],
            "workspace_path": root.path().to_string_lossy(),
            "no_diff_expected": true,
        }),
    )
    .expect_err("untagged no-diff completion must fail");

    assert!(error.to_string().contains("no-diff-expected tag"));
    assert_eq!(host.task_status("T1"), TaskStatus::Review);
}

/// [AC6] The transition records who authorized it and preserves the ship
/// attribution of whoever actually implemented the work.
#[test]
fn completion_records_authorization_provenance_and_keeps_ship_attribution() {
    let (root, host) = host(vec![review_batch_task("T1", Some("claude"), Some("codex"))]);
    host.queue_pr_status([merged_state()]);

    let mut input = complete_input(root.path(), &["T1"]);
    input["authorized_by"] = json!("operator-jane");
    let output = pr_complete(&host, &input).expect("complete");

    assert!(
        output["authorization"]
            .as_str()
            .expect("authorization string")
            .contains("operator-jane")
    );
    let updates = host.activity_updates();
    let (task_id, update) = updates.last().expect("a completion update was applied");
    assert_eq!(task_id, "T1");
    assert_eq!(update.status, TaskStatus::Done);
    let note = update.note.as_deref().expect("provenance note");
    assert!(note.contains("operator-jane"), "note: {note}");
    assert!(note.contains("batch-1"), "note must name the run: {note}");
    assert_eq!(
        update.model.as_deref(),
        Some("claude"),
        "completion must not overwrite the implementer's ship attribution"
    );
}

/// [AC6] Completion authority never substitutes for backlog approval: a task
/// that has not been delivered to `review` cannot be completed.
#[test]
fn completion_refuses_any_task_that_is_not_in_review() {
    for status in [
        TaskStatus::Proposed,
        TaskStatus::Backlog,
        TaskStatus::InProgress,
        TaskStatus::Blocked,
    ] {
        let mut task = review_batch_task("T1", None, None);
        task.status = status;
        let (_root, host) = host(vec![task]);

        let error = task_complete(
            &host,
            &json!({ "job_run_id": "batch-1", "task_ids": ["T1"] }),
        )
        .err()
        .unwrap_or_else(|| panic!("{status} must not be completable"));

        assert!(
            error.to_string().contains("must be in review"),
            "{status}: unexpected error: {error}"
        );
        assert_eq!(host.task_status("T1"), status);
    }
}

/// Completion is idempotent, so a resumed run does not fail on work it already
/// finished.
#[test]
fn completing_an_already_done_task_is_a_skip_not_a_failure() {
    let mut task = review_batch_task("T1", None, None);
    task.status = TaskStatus::Done;
    let (_root, host) = host(vec![task]);

    let output = task_complete(
        &host,
        &json!({ "job_run_id": "batch-1", "task_ids": ["T1"] }),
    )
    .expect("idempotent completion");

    assert_eq!(output["skipped_task_ids"], json!(["T1"]));
    assert_eq!(output["completed_task_ids"], json!([]));
    assert!(
        host.activity_updates().is_empty(),
        "an already-done task must not be rewritten"
    );
}

/// Completion never writes a review verdict: the operator authorized delivery,
/// not an independent approval.
#[test]
fn completion_does_not_fabricate_a_review_verdict() {
    let mut task = review_batch_task("T1", None, None);
    task.pr_status = None;
    let (root, host) = host(vec![task]);
    host.queue_pr_status([merged_state()]);

    pr_complete(&host, &complete_input(root.path(), &["T1"])).expect("complete");

    let tasks_pr_status = host
        .activity_updates()
        .into_iter()
        .all(|(_, update)| update.comment.is_none());
    assert!(tasks_pr_status);
    assert!(
        host.automation_updates().is_empty(),
        "completion must not stamp a pr_status review decision"
    );
}
