//! Completion-authorized delivery [ORB-11187].
//!
//! Every case drives the real `pr_complete` / `task_complete` code against the
//! shared fake host with a scripted sequence of `pr.status` answers, so the
//! merge-state machine is exercised without GitHub.

use std::fs;
use std::process::Command;

use orbit_common::OrbitError;
use orbit_types::task::{NO_DIFF_EXPECTED_TAG, Task, TaskStatus};
use serde_json::{Value, json};
use tempfile::tempdir;

use super::super::super::super::task_update::task_complete;
use super::super::complete::pr_complete;
use super::test_support::{
    PR_MERGE_CAPABILITIES_OPERATION, PR_MERGE_OPERATION, PR_STATUS_OPERATION, PUSH_OPERATION,
    PrOpenTestHost, git, rebase_conflict_pr_workspace, review_batch_task,
};
use crate::context::{RuntimeHost, TaskActivityUpdate};

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
    input["max_wait_seconds"] = json!(10);
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
fn blocked_running_checks_are_repolled_and_merge_when_clean() {
    let (root, host) = host(vec![review_batch_task("T1", None, None)]);
    let mut blocked = state("BLOCKED");
    blocked["reviewDecision"] = Value::Null;
    blocked["statusCheckRollup"] = json!([
        {"__typename": "CheckRun", "name": "linux", "status": "IN_PROGRESS", "conclusion": null}
    ]);
    host.queue_pr_status([blocked, state("CLEAN"), merged_state()]);
    host.queue_merge_capabilities_with_auto_merge(true, true, true, true, false);

    let mut input = complete_input(root.path(), &["T1"]);
    input["max_wait_seconds"] = json!(10);
    let output = pr_complete(&host, &input).expect("complete after BLOCKED checks settle");

    assert_eq!(output["merge"]["merged"], true);
    assert_eq!(output["merge"]["auto_merge_requested"], false);
    assert_eq!(output["merge"]["waited_seconds"], 5);
    assert_eq!(
        status_reads(&host),
        3,
        "BLOCKED must be polled again before merging"
    );
    assert_eq!(merge_calls(&host).len(), 1);
    assert_eq!(host.task_status("T1"), TaskStatus::Done);
}

/// The `gh pr view --json` projection of a protected PR whose required checks
/// have just started. `gh` serializes the absent review decision as `""`, not
/// the GraphQL `null` [ORB-13759].
const GH_BLOCKED_CHECKS_STARTING: &str = r#"{
  "baseRefName": "agent-main",
  "headRefName": "orbit/test-batch",
  "headRefOid": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "mergeCommit": null,
  "mergeStateStatus": "BLOCKED",
  "mergeable": "MERGEABLE",
  "mergedAt": null,
  "number": 42,
  "reviewDecision": "",
  "state": "OPEN",
  "statusCheckRollup": [
    {"__typename": "CheckRun", "completedAt": "0001-01-01T00:00:00Z", "conclusion": "", "detailsUrl": "https://github.com/o/r/actions/runs/1/job/1", "name": "test", "startedAt": "2026-10-03T03:20:29Z", "status": "IN_PROGRESS", "workflowName": "CI"},
    {"__typename": "CheckRun", "completedAt": "0001-01-01T00:00:00Z", "conclusion": "", "detailsUrl": "https://github.com/o/r/actions/runs/1/job/2", "name": "lint", "startedAt": "0001-01-01T00:00:00Z", "status": "QUEUED", "workflowName": "CI"}
  ],
  "url": "https://github.com/o/r/pull/42"
}"#;

fn gh_blocked_checks_starting() -> Value {
    serde_json::from_str(GH_BLOCKED_CHECKS_STARTING).expect("gh-shaped observation")
}

/// [ORB-13759] The recorded incident: a completion-authorized run reads its
/// freshly published PR as BLOCKED with gh's empty review decision while
/// checks start. It waits within its budget, then makes the permitted
/// conditional merge of the pinned candidate and verifies delivery.
#[test]
fn gh_empty_review_decision_with_starting_checks_waits_then_merges_the_pinned_candidate() {
    let (root, host) = host(vec![review_batch_task("T1", None, None)]);
    host.queue_pr_status([
        gh_blocked_checks_starting(),
        open_at("CLEAN", PUBLISHED_HEAD_SHA),
        merged_state(),
    ]);
    host.queue_merge_capabilities_with_auto_merge(true, true, true, true, true);
    let mut input = published_input(root.path());
    input["max_wait_seconds"] = json!(10);

    let output = pr_complete(&host, &input).expect("wait for checks, then deliver");

    assert_eq!(output["merge"]["merged"], true);
    assert_eq!(output["merge"]["waited_seconds"], 5);
    assert_eq!(output["merge"]["auto_merge_requested"], false);
    assert_eq!(
        output["merge"]["delivery_evidence"]["head_sha"],
        PUBLISHED_HEAD_SHA
    );
    assert_eq!(status_reads(&host), 3, "BLOCKED is polled again");
    let merges = merge_calls(&host);
    assert_eq!(merges.len(), 1, "one ordinary merge once checks settle");
    assert_eq!(merges[0]["auto"], false);
    assert_eq!(merges[0]["reviewed_head_sha"], PUBLISHED_HEAD_SHA);
    assert!(
        merges[0].get("admin").is_none(),
        "completion must never request an administrative bypass"
    );
    assert_eq!(output["completed_task_ids"], json!(["T1"]));
    assert_eq!(host.task_status("T1"), TaskStatus::Done);
}

/// [ORB-13759] An empty review decision only permits waiting on checks that
/// are provably in flight. Settled checks, failures, unknown review shapes and
/// a moved head still refuse before any merge request.
#[test]
fn gh_empty_review_decision_never_merges_on_its_own() {
    let mut settled = gh_blocked_checks_starting();
    settled["statusCheckRollup"] = json!([
        {"__typename": "CheckRun", "name": "test", "status": "COMPLETED", "conclusion": "SUCCESS"}
    ]);
    let mut failed = gh_blocked_checks_starting();
    failed["statusCheckRollup"][1] = json!({"__typename": "CheckRun", "name": "lint", "status": "COMPLETED", "conclusion": "FAILURE"});
    let mut missing_review = gh_blocked_checks_starting();
    missing_review
        .as_object_mut()
        .expect("object")
        .remove("reviewDecision");
    let mut unknown_review = gh_blocked_checks_starting();
    unknown_review["reviewDecision"] = json!("DISMISSED");
    let mut moved_head = gh_blocked_checks_starting();
    moved_head["headRefOid"] = json!(MOVED_HEAD_SHA);

    for (case, status, expected) in [
        (
            "settled",
            settled,
            "required reviews or checks are not satisfied",
        ),
        ("failed", failed, "status check 'lint' failed"),
        ("missing", missing_review, "review decision is unavailable"),
        ("unknown", unknown_review, "review decision is unavailable"),
        ("moved", moved_head, "delivery_evidence_stale"),
    ] {
        let (root, host) = host(vec![review_batch_task("T1", None, None)]);
        host.queue_pr_status([status]);
        host.queue_merge_capabilities_with_auto_merge(true, true, true, true, true);

        let error = pr_complete(&host, &published_input(root.path()))
            .expect_err("an empty review decision alone must not merge");

        assert!(error.to_string().contains(expected), "{case}: {error}");
        assert!(merge_calls(&host).is_empty(), "{case}: no merge request");
        assert_eq!(host.task_status("T1"), TaskStatus::Review, "{case}");
    }
}

#[test]
fn blocked_failed_check_and_required_review_never_request_a_merge() {
    for (check, review, expected) in [
        (
            json!({"name": "macos", "status": "COMPLETED", "conclusion": "FAILURE"}),
            Value::Null,
            "macos",
        ),
        (
            json!({"name": "linux", "status": "IN_PROGRESS", "conclusion": null}),
            json!("REVIEW_REQUIRED"),
            "REVIEW_REQUIRED",
        ),
    ] {
        let (root, host) = host(vec![review_batch_task("T1", None, None)]);
        let mut blocked = state("BLOCKED");
        blocked["reviewDecision"] = review;
        blocked["statusCheckRollup"] = json!([check]);
        host.queue_pr_status([blocked]);

        let error = pr_complete(&host, &complete_input(root.path(), &["T1"]))
            .expect_err("a failed check or required review must refuse completion");
        assert!(error.to_string().contains(expected), "{error}");
        assert!(merge_calls(&host).is_empty());
        assert_eq!(host.task_status("T1"), TaskStatus::Review);
    }
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

#[test]
fn zero_poll_interval_is_clamped_and_does_not_hammer_pr_status() {
    let (root, host) = host(vec![review_batch_task("T1", None, None)]);
    host.queue_pr_status([state("PENDING")]);
    host.queue_merge_capabilities_with_auto_merge(true, true, true, true, false);

    let mut input = complete_input(root.path(), &["T1"]);
    input["poll_interval_seconds"] = json!(0);
    input["max_wait_seconds"] = json!(5);
    let error = pr_complete(&host, &input).expect_err("pending checks must time out");

    assert!(error.to_string().contains("timed out"), "{error}");
    let status_reads = host
        .vcs_calls()
        .iter()
        .filter(|call| call.operation == PR_STATUS_OPERATION)
        .count();
    assert!(
        status_reads <= 1,
        "a zero input interval must be clamped before polling"
    );
}

#[test]
fn excessive_wait_budget_is_reported_as_the_clamped_ceiling() {
    let (root, host) = host(vec![review_batch_task("T1", None, None)]);
    host.queue_pr_status([merged_state()]);

    let mut input = complete_input(root.path(), &["T1"]);
    input["max_wait_seconds"] = json!(10_000_000);
    let output = pr_complete(&host, &input).expect("an already merged PR completes");

    assert_eq!(output["merge"]["max_wait_seconds"], json!(6 * 60 * 60));
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

/// The merge projection `gh pr view` actually returns for a merged PR. The
/// completion gate reads every one of these fields, so under-specifying the
/// fixture would hide the identity and merge-evidence checks it exists for.
fn merged_state() -> Value {
    merged_state_for("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
}

fn merged_state_for(head_sha: &str) -> Value {
    json!({
        "number": 42,
        "state": "MERGED",
        "mergedAt": "2026-09-05T00:00:00Z",
        "headRefName": "orbit/test-batch",
        "baseRefName": "agent-main",
        "headRefOid": head_sha,
        "mergeCommit": { "oid": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" },
    })
}

fn state(merge_state_status: &str) -> Value {
    json!({
        "number": 42,
        "state": "OPEN",
        "mergedAt": Value::Null,
        "mergeStateStatus": merge_state_status,
        "headRefName": "orbit/test-batch",
        "baseRefName": "agent-main",
    })
}

fn merge_calls(host: &PrOpenTestHost) -> Vec<Value> {
    host.vcs_calls()
        .into_iter()
        .filter(|call| call.operation == PR_MERGE_OPERATION)
        .map(|call| call.input)
        .collect()
}

fn status_reads(host: &PrOpenTestHost) -> usize {
    host.vcs_calls()
        .into_iter()
        .filter(|call| call.operation == PR_STATUS_OPERATION)
        .count()
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

/// The candidate this run published. A later head is somebody else's delivery.
const PUBLISHED_HEAD_SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const MOVED_HEAD_SHA: &str = "cccccccccccccccccccccccccccccccccccccccc";

fn published_input(workspace_path: &std::path::Path) -> Value {
    let mut input = complete_input(workspace_path, &["T1"]);
    input["completion"] = json!("done");
    input["head"] = json!("orbit/test-batch");
    input["published_head_sha"] = json!(PUBLISHED_HEAD_SHA);
    input["base"] = json!("agent-main");
    input
}

fn open_at(merge_state_status: &str, head_sha: &str) -> Value {
    let mut status = state(merge_state_status);
    status["headRefOid"] = json!(head_sha);
    status
}

/// [ORB-13444] A published head of A and a live head of B is refused before
/// any merge or auto-merge, whether GitHub currently calls the PR clean or
/// still pending.
#[test]
fn published_head_mismatch_refuses_clean_and_pending_before_any_merge() {
    for merge_state in ["CLEAN", "PENDING"] {
        let (root, host) = host(vec![review_batch_task("T1", None, None)]);
        host.queue_pr_status([open_at(merge_state, MOVED_HEAD_SHA)]);
        host.queue_merge_capabilities_with_auto_merge(true, true, true, true, true);

        let error = pr_complete(&host, &published_input(root.path()))
            .expect_err("a moved published head must not be merged");

        let message = error.to_string();
        assert!(
            message.contains("delivery_evidence_stale"),
            "{merge_state}: {message}"
        );
        assert!(message.contains(MOVED_HEAD_SHA), "{merge_state}: {message}");
        assert!(
            message.contains(PUBLISHED_HEAD_SHA),
            "{merge_state}: {message}"
        );
        assert!(
            merge_calls(&host).is_empty(),
            "{merge_state} must not request a merge or auto-merge"
        );
        assert_eq!(host.task_status("T1"), TaskStatus::Review);
    }
}

/// [ORB-13444] The authorized published head still completes, and the merge
/// request carries that SHA so the provider can refuse a later move.
#[test]
fn published_head_completes_through_the_conditional_merge() {
    let (root, host) = host(vec![review_batch_task("T1", None, None)]);
    host.queue_pr_status([open_at("CLEAN", PUBLISHED_HEAD_SHA), merged_state()]);

    let output = pr_complete(&host, &published_input(root.path())).expect("authorized head");

    assert_eq!(output["merge"]["merged"], true);
    assert_eq!(output["merge"]["auto_merge_requested"], false);
    let merges = merge_calls(&host);
    assert_eq!(merges.len(), 1);
    assert_eq!(merges[0]["auto"], false);
    assert_eq!(merges[0]["reviewed_head_sha"], PUBLISHED_HEAD_SHA);
    assert_eq!(host.task_status("T1"), TaskStatus::Done);
    assert!(
        host.review_landings().is_empty(),
        "an ungated published head is not a before-PR review landing"
    );
}

/// [ORB-13444] Pending checks on the published head wait for the synchronous
/// mutation. Auto-merge cannot keep the candidate condition.
#[test]
fn published_pending_head_waits_for_the_conditional_merge() {
    let (root, host) = host(vec![review_batch_task("T1", None, None)]);
    host.queue_pr_status([
        open_at("PENDING", PUBLISHED_HEAD_SHA),
        open_at("CLEAN", PUBLISHED_HEAD_SHA),
        merged_state(),
    ]);
    host.queue_merge_capabilities_with_auto_merge(true, true, true, true, true);
    let mut input = published_input(root.path());
    input["max_wait_seconds"] = json!(10);

    let output = pr_complete(&host, &input).expect("wait, then merge the published head");

    assert_eq!(output["merge"]["merged"], true);
    assert_eq!(output["merge"]["auto_merge_requested"], false);
    let merges = merge_calls(&host);
    assert_eq!(
        merges.len(),
        1,
        "auto-merge is not a substitute for the pin"
    );
    assert_eq!(merges[0]["auto"], false);
    assert_eq!(merges[0]["reviewed_head_sha"], PUBLISHED_HEAD_SHA);
    assert_eq!(host.task_status("T1"), TaskStatus::Done);
}

/// [ORB-13444] The status read saw A, then the provider head moved to B
/// before the mutation. The conditional request refuses B and does not merge.
#[cfg(unix)]
#[test]
fn published_head_race_after_the_read_is_refused_by_the_provider() {
    use super::super::super::tests::with_fake_gh;

    let script = r#"#!/bin/sh
set -eu
printf '%s\n' "$@" >> provider-args
if [ "$1 $2" = "pr view" ]; then
    if [ -f provider-merged ]; then
        printf '%s\n' '{"state":"MERGED","headRefName":"orbit/test-batch","baseRefName":"agent-main","headRefOid":"2222222222222222222222222222222222222222","mergeCommit":{"oid":"unvalidated-merge"}}'
    else
        printf '%s\n' '{"state":"OPEN","mergeStateStatus":"CLEAN","headRefName":"orbit/test-batch","baseRefName":"agent-main","headRefOid":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}'
    fi
    exit 0
fi
printf '%s' '2222222222222222222222222222222222222222' > provider-head
if [ "$1" = "api" ]; then
    for arg in "$@"; do
        case "$arg" in
            sha=*) if [ "${arg#sha=}" != "$(cat provider-head)" ]; then
                echo 'HTTP 409: Head branch was modified' >&2
                exit 1
            fi ;;
        esac
    done
fi
printf '%s' 'merged' > provider-merged
printf '%s\n' '{"merged":true,"sha":"unvalidated-merge"}'
"#;
    if !with_fake_gh(
        module_path!(),
        "published_head_race_after_the_read_is_refused_by_the_provider",
        script,
    ) {
        return;
    }
    let (root, host) = host(vec![review_batch_task("T1", None, None)]);
    let host = host.with_provider_completion();
    let error = pr_complete(&host, &published_input(root.path()))
        .expect_err("provider must reject a head that moved after the read");

    assert!(error.to_string().contains("HTTP 409"), "{error}");
    assert!(!root.path().join("provider-merged").exists());
    assert_eq!(host.task_status("T1"), TaskStatus::Review);
    assert!(host.review_landings().is_empty());
    let args = fs::read_to_string(root.path().join("provider-args")).expect("provider args");
    let expected_mutation = format!(
        "api\nrepos/{{owner}}/{{repo}}/pulls/42/merge\n--method\nPUT\n-f\nsha={PUBLISHED_HEAD_SHA}\n-f\nmerge_method=squash\n"
    );
    assert!(args.contains(&expected_mutation), "{args}");
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
        input["max_wait_seconds"] = json!(10);

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

/// The completion race that motivated ORB-11488: the candidate was already
/// published, then the target base changed on the same path before merge.
/// The first completion attempt must leave a proven, stopped rebase; after the
/// bounded recovery leaf resolves it, retrying completion reuses that rewrite,
/// lease-pushes the same branch, and merges the same PR.
#[test]
fn published_pr_conflict_reuses_pinned_rebase_branch_and_pr_on_completion_retry() {
    let workspace = rebase_conflict_pr_workspace();
    let published_head_sha = git(&workspace.repo, &["rev-parse", "orbit/test-batch"]);
    let host = PrOpenTestHost::new(
        vec![review_batch_task("T1", None, None)],
        workspace.repo.clone(),
    );
    host.queue_pr_status([state("DIRTY")]);
    let mut input = complete_input(&workspace.repo, &["T1"]);
    input["completion"] = json!("done");
    input["head"] = json!("orbit/test-batch");
    input["published_head_sha"] = json!(published_head_sha);
    input["base"] = json!("agent-main");
    input["base_sync"] = json!("remote");

    let error = pr_complete(&host, &input).expect_err("advanced base must conflict");
    let OrbitError::RecoverableVcsConflict(conflict) = error else {
        panic!("completion conflict must stay typed: {error}");
    };
    assert_eq!(conflict.operation, "git_rebase");
    assert_eq!(conflict.conflicting_paths, vec!["src/lib.rs"]);
    assert_eq!(host.task_status("T1"), TaskStatus::Review);

    fs::write(
        workspace.repo.join("src/lib.rs"),
        "pub fn diverged() {}\npub fn branch() {}\n",
    )
    .expect("resolve both sides of the completion conflict");
    git(&workspace.repo, &["add", "src/lib.rs"]);
    let continued = Command::new("git")
        .args(["-c", "core.editor=true", "rebase", "--continue"])
        .current_dir(&workspace.repo)
        .output()
        .expect("continue completion rebase");
    assert!(
        continued.status.success(),
        "rebase continue failed: {}",
        String::from_utf8_lossy(&continued.stderr)
    );

    let recovered_head_sha = git(&workspace.repo, &["rev-parse", "HEAD"]);
    let run_id = input["job_run_id"].as_str().unwrap();
    crate::context::RuntimeHost::checkpoint_rebase_recovery(
        &host,
        run_id,
        "complete_pr",
        &json!({
            "run_id": run_id,
            "step_id": "complete_pr",
            "workspace_path": workspace.repo,
            "task_ids": ["T1"],
            "head": input["head"],
            "head_sha_before": published_head_sha,
            "original_base_sha": conflict.original_base_sha,
            "base_ref": "refs/remotes/origin/agent-main",
            "base_sha": conflict.target_base_sha,
            "remote_sha_before": published_head_sha,
            "head_sha": recovered_head_sha,
            "rewritten": true,
        }),
    )
    .unwrap();

    let mut recovered_clean = state("CLEAN");
    recovered_clean["headRefOid"] = json!(recovered_head_sha);
    host.queue_pr_status([
        state("DIRTY"),
        recovered_clean,
        merged_state_for(&recovered_head_sha),
    ]);
    let output = pr_complete(&host, &input).expect("retry merges recovered published PR");

    assert_eq!(output["merge"]["pr_number"], "42");
    assert_eq!(host.task_status("T1"), TaskStatus::Done);
    assert_eq!(
        fs::read_to_string(workspace.repo.join("src/lib.rs")).expect("recovered file"),
        "pub fn diverged() {}\npub fn branch() {}\n"
    );
    let pushes = host
        .vcs_calls()
        .into_iter()
        .filter(|call| call.operation == PUSH_OPERATION)
        .collect::<Vec<_>>();
    assert_eq!(pushes.len(), 1, "recovery retry pushes exactly once");
    assert_eq!(pushes[0].input["branch"], "orbit/test-batch");
    assert_eq!(pushes[0].input["force_with_lease"], true);
    let merges = merge_calls(&host);
    assert_eq!(merges.len(), 1, "the existing PR merges once");
    assert_eq!(merges[0]["auto"], false);
    assert_eq!(
        merges[0]["reviewed_head_sha"],
        recovered_head_sha.as_str(),
        "the repaired candidate, not the pre-repair SHA, conditions the merge"
    );
}

#[test]
fn completion_conflict_refuses_a_concurrently_updated_published_branch() {
    let workspace = rebase_conflict_pr_workspace();
    let published_head_sha = git(&workspace.repo, &["rev-parse", "orbit/test-batch"]);
    fs::write(workspace.repo.join("candidate-extra.txt"), "concurrent\n")
        .expect("write concurrent candidate update");
    git(&workspace.repo, &["add", "candidate-extra.txt"]);
    git(
        &workspace.repo,
        &["commit", "-m", "concurrent candidate update"],
    );
    git(&workspace.repo, &["push", "origin", "orbit/test-batch"]);
    let host = PrOpenTestHost::new(
        vec![review_batch_task("T1", None, None)],
        workspace.repo.clone(),
    );
    host.queue_pr_status([state("DIRTY")]);
    let mut input = complete_input(&workspace.repo, &["T1"]);
    input["completion"] = json!("done");
    input["head"] = json!("orbit/test-batch");
    input["published_head_sha"] = json!(published_head_sha);
    input["base"] = json!("agent-main");
    input["base_sync"] = json!("remote");

    let error = pr_complete(&host, &input).expect_err("stale branch ownership must fail closed");

    assert!(
        error
            .to_string()
            .contains("moved away from completion checkpoint"),
        "{error}"
    );
    assert!(!matches!(error, OrbitError::RecoverableVcsConflict(_)));
    assert_eq!(host.task_status("T1"), TaskStatus::Review);
    assert!(
        host.vcs_calls()
            .iter()
            .all(|call| call.operation != PUSH_OPERATION),
        "a stale owner must not push"
    );
}

#[test]
fn second_base_advance_after_recovery_does_not_start_another_rebase() {
    let workspace = rebase_conflict_pr_workspace();
    let published_head_sha = git(&workspace.repo, &["rev-parse", "orbit/test-batch"]);
    let host = PrOpenTestHost::new(
        vec![review_batch_task("T1", None, None)],
        workspace.repo.clone(),
    );
    host.queue_pr_status([state("DIRTY")]);
    let mut input = complete_input(&workspace.repo, &["T1"]);
    input["completion"] = json!("done");
    input["head"] = json!("orbit/test-batch");
    input["published_head_sha"] = json!(published_head_sha);
    input["base"] = json!("agent-main");
    input["base_sync"] = json!("remote");

    let first = pr_complete(&host, &input).expect_err("first base advance conflicts");
    assert!(matches!(first, OrbitError::RecoverableVcsConflict(_)));
    fs::write(
        workspace.repo.join("src/lib.rs"),
        "pub fn diverged() {}\npub fn branch() {}\n",
    )
    .expect("resolve first conflict");
    git(&workspace.repo, &["add", "src/lib.rs"]);
    git(
        &workspace.repo,
        &["-c", "core.editor=true", "rebase", "--continue"],
    );

    git(&workspace.repo, &["checkout", "agent-main"]);
    fs::write(
        workspace.repo.join("src/lib.rs"),
        "pub fn advanced_again() {}\n",
    )
    .expect("advance base again");
    git(&workspace.repo, &["add", "src/lib.rs"]);
    git(&workspace.repo, &["commit", "-m", "second base advance"]);
    git(&workspace.repo, &["push", "origin", "agent-main"]);
    git(&workspace.repo, &["checkout", "orbit/test-batch"]);
    host.queue_pr_status([state("DIRTY")]);

    let second = pr_complete(&host, &input).expect_err("moving target must fail closed");

    assert!(
        second
            .to_string()
            .contains("branch state no longer matches the durable pre-rewrite checkpoint"),
        "{second}"
    );
    assert!(!matches!(second, OrbitError::RecoverableVcsConflict(_)));
    assert_eq!(host.task_status("T1"), TaskStatus::Review);
    assert!(
        host.vcs_calls()
            .iter()
            .all(|call| call.operation != PUSH_OPERATION),
        "a second base advance must not push"
    );
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
    assert_eq!(update.calling_run_id.as_deref(), Some("batch-1"));
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

#[test]
fn automation_completion_preserves_the_live_implementation_run_error() {
    struct LiveRunHost(Task);

    impl RuntimeHost for LiveRunHost {
        fn get_task(&self, task_id: &str) -> Result<Task, OrbitError> {
            assert_eq!(task_id, self.0.id);
            Ok(self.0.clone())
        }

        fn update_task_from_activity(
            &self,
            task_id: &str,
            update: TaskActivityUpdate,
        ) -> Result<Task, OrbitError> {
            assert_eq!(task_id, self.0.id);
            assert_eq!(update.calling_run_id.as_deref(), Some("batch-1"));
            Err(OrbitError::TaskCompletionLiveRun {
                task_id: task_id.to_string(),
                run_id: "jrun-implementation".to_string(),
            })
        }
    }

    let host = LiveRunHost(review_batch_task("T1", None, None));
    let error = task_complete(&host, &json!({"job_run_id": "batch-1", "task_id": "T1"}))
        .expect_err("automation completion must preserve the live run refusal");
    assert!(matches!(
        error,
        OrbitError::TaskCompletionLiveRun { task_id, run_id }
            if task_id == "T1" && run_id == "jrun-implementation"
    ));
    assert_eq!(host.0.status, TaskStatus::Review);
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
