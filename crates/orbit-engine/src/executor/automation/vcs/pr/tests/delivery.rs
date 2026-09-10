//! Fresh delivery evidence before automatic completion [ORB-11982].
//!
//! These cases drive the real `pr_complete` against scripted `pr.status`
//! answers that reproduce the F2026-09-102 shape: a base-modification race
//! around an already-published candidate. The rule under test is that the
//! automatic path completes only on evidence it read at completion time about
//! the candidate this run itself published.

use orbit_types::task::TaskStatus;
use serde_json::{Value, json};

use super::super::complete::pr_complete;
use super::test_support::{
    PR_MERGE_OPERATION, PR_STATUS_OPERATION, PrOpenTestHost, PrWorkspace, git, pr_workspace,
    rebase_conflict_pr_workspace, review_batch_task,
};

/// The candidate the pipeline published for this bundle. Completion is
/// authorized to deliver this and, after an in-run repair, its rewrite — never
/// an unrelated head.
const PUBLISHED_HEAD_SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const FOREIGN_HEAD_SHA: &str = "cccccccccccccccccccccccccccccccccccccccc";
const LANDED_COMMIT_SHA: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

/// [AC1] The recorded incident shape: the provider still reports the pull
/// request as open while carrying a merge timestamp. Completion refuses the
/// contradiction instead of reading the half that would close the run.
#[test]
fn an_open_pull_request_reporting_a_merge_time_cannot_reach_done() {
    let workspace = pr_workspace();
    let host = pinned_host(&workspace);
    host.queue_pr_status([json!({
        "number": 42,
        "state": "OPEN",
        "mergedAt": "2026-09-09T01:23:08Z",
        "mergeStateStatus": "CLEAN",
        "headRefName": "orbit/test-batch",
        "baseRefName": "agent-main",
        "headRefOid": PUBLISHED_HEAD_SHA,
    })]);

    let error = pr_complete(&host, &pinned_input(&workspace))
        .expect_err("an open pull request must not complete");

    let message = error.to_string();
    assert!(message.contains("contradictory merge state"), "{message}");
    assert!(message.contains("state 'OPEN'"), "{message}");
    assert_eq!(host.task_status("T1"), TaskStatus::Review);
    assert!(
        merge_calls(&host).is_empty(),
        "a contradictory state must not be merged through"
    );
}

/// [AC1] A base-modification race that repoints the pull request at another
/// base is a different delivery. It is refused before any merge is requested.
#[test]
fn a_pull_request_retargeted_onto_another_base_is_never_merged() {
    let workspace = pr_workspace();
    let host = pinned_host(&workspace);
    host.queue_pr_status([json!({
        "number": 42,
        "state": "OPEN",
        "mergedAt": Value::Null,
        "mergeStateStatus": "CLEAN",
        "headRefName": "orbit/test-batch",
        "baseRefName": "release-1",
        "headRefOid": PUBLISHED_HEAD_SHA,
    })]);

    let error =
        pr_complete(&host, &pinned_input(&workspace)).expect_err("a retargeted PR must not merge");

    let message = error.to_string();
    assert!(message.contains("delivery_evidence_stale"), "{message}");
    assert!(message.contains("baseRefName 'release-1'"), "{message}");
    assert_eq!(host.task_status("T1"), TaskStatus::Review);
    assert!(merge_calls(&host).is_empty());
}

/// [AC1] A merge of some other head is somebody else's delivery. The run
/// validated one candidate and may only complete on that one.
#[test]
fn a_merge_of_an_unpublished_head_does_not_complete_the_bundle() {
    let workspace = pr_workspace();
    let host = pinned_host(&workspace);
    host.queue_pr_status([merged_state(FOREIGN_HEAD_SHA)]);

    let error = pr_complete(&host, &pinned_input(&workspace))
        .expect_err("an unpublished head must not complete");

    let message = error.to_string();
    assert!(message.contains("delivery_evidence_stale"), "{message}");
    assert!(message.contains(FOREIGN_HEAD_SHA), "{message}");
    assert!(message.contains(PUBLISHED_HEAD_SHA), "{message}");
    assert_eq!(host.task_status("T1"), TaskStatus::Review);
}

/// [AC1] A merged state without the commit that carried it is not evidence of
/// what landed, so it cannot authorize the transition.
#[test]
fn a_merged_state_without_a_merge_commit_is_not_delivery_evidence() {
    let workspace = pr_workspace();
    let host = pinned_host(&workspace);
    let mut status = merged_state(PUBLISHED_HEAD_SHA);
    status["mergeCommit"] = Value::Null;
    host.queue_pr_status([status]);

    let error = pr_complete(&host, &pinned_input(&workspace))
        .expect_err("an unattributed merge must not complete");

    assert!(
        error.to_string().contains("without a merge commit"),
        "{error}"
    );
    assert_eq!(host.task_status("T1"), TaskStatus::Review);
}

/// [AC1] A retry re-reads the provider. The clean state an earlier attempt saw
/// before its merge request failed carries no authority into the next attempt.
#[test]
fn a_clean_reading_from_a_failed_attempt_is_not_reused_by_the_retry() {
    let workspace = pr_workspace();
    let host = pinned_host(&workspace);
    host.queue_pr_status([open_state("CLEAN")]);
    host.queue_vcs_error(PR_MERGE_OPERATION, "gh: merge permission denied");

    let first = pr_complete(&host, &pinned_input(&workspace))
        .expect_err("a denied merge must fail the attempt");
    assert!(first.to_string().contains("merge permission denied"));
    assert_eq!(host.task_status("T1"), TaskStatus::Review);

    // The base moved while the first attempt was failing.
    host.queue_pr_status([open_state("BLOCKED")]);
    let retry = pr_complete(&host, &pinned_input(&workspace))
        .expect_err("the retry must judge the current state");

    assert!(
        retry.to_string().contains("branch protection"),
        "the retry re-read the provider rather than reusing the clean state: {retry}"
    );
    assert_eq!(host.task_status("T1"), TaskStatus::Review);
    assert_eq!(
        status_reads(&host),
        2,
        "each attempt reads the pull request for itself"
    );
}

/// [AC2] The authorized delivery: a fresh merged read of the published
/// candidate completes the bundle, and the evidence that permitted it is
/// persisted on the activity output and in the durable authorization note.
#[test]
fn fresh_delivery_evidence_completes_the_bundle_and_is_persisted() {
    let workspace = pr_workspace();
    let host = pinned_host(&workspace);
    host.queue_pr_status([merged_state(PUBLISHED_HEAD_SHA)]);

    let output = pr_complete(&host, &pinned_input(&workspace)).expect("authorized delivery");

    assert_eq!(
        output["merge"]["delivery_evidence"],
        json!({
            "pr_number": "42",
            "merged_at": "2026-09-09T01:23:08Z",
            "head_ref": "orbit/test-batch",
            "base_ref": "agent-main",
            "head_sha": PUBLISHED_HEAD_SHA,
            "merge_commit": LANDED_COMMIT_SHA,
        })
    );
    assert_eq!(host.task_status("T1"), TaskStatus::Done);

    let authorization = output["authorization"].as_str().expect("authorization");
    assert!(
        authorization.contains("delivered by pull request #42"),
        "{authorization}"
    );
    assert!(authorization.contains(LANDED_COMMIT_SHA), "{authorization}");
    let (_, update) = host
        .activity_updates()
        .into_iter()
        .next_back()
        .expect("completion update");
    let note = update.note.as_deref().expect("provenance note");
    assert!(
        note.contains(LANDED_COMMIT_SHA),
        "task history must name the evidence that permitted done: {note}"
    );
}

/// [AC2] Recovering the same delivered bundle again re-verifies the same
/// evidence and changes nothing: no second merge request, no rewritten task.
#[test]
fn repeating_completion_after_delivery_is_idempotent() {
    let workspace = pr_workspace();
    let host = pinned_host(&workspace);
    host.queue_pr_status([merged_state(PUBLISHED_HEAD_SHA)]);
    pr_complete(&host, &pinned_input(&workspace)).expect("first authorized delivery");
    let updates_after_delivery = host.activity_updates().len();

    host.queue_pr_status([merged_state(PUBLISHED_HEAD_SHA)]);
    let repeated = pr_complete(&host, &pinned_input(&workspace)).expect("idempotent recovery");

    assert_eq!(repeated["skipped_task_ids"], json!(["T1"]));
    assert_eq!(repeated["completed_task_ids"], json!([]));
    assert_eq!(
        repeated["merge"]["delivery_evidence"]["merge_commit"],
        LANDED_COMMIT_SHA
    );
    assert_eq!(host.task_status("T1"), TaskStatus::Done);
    assert!(merge_calls(&host).is_empty(), "the PR was already merged");
    assert_eq!(
        host.activity_updates().len(),
        updates_after_delivery,
        "an already-done task must not be rewritten"
    );
}

/// [AC3] The friction's confusion, made legible: required checks that ran on a
/// merge ref built from an older base do not describe the current candidate.
/// The refusal says so and names the base the candidate is behind.
#[test]
fn blocked_checks_behind_an_advanced_base_are_reported_as_a_stale_merge_ref() {
    let workspace = rebase_conflict_pr_workspace();
    let published_head_sha = git(&workspace.repo, &["rev-parse", "HEAD"]);
    let host = pinned_host(&workspace);
    host.queue_pr_status([open_state("BLOCKED")]);
    let mut input = pinned_input(&workspace);
    input["published_head_sha"] = json!(published_head_sha);

    let error = pr_complete(&host, &input).expect_err("blocked checks must fail the run");

    let message = error.to_string();
    assert!(message.contains("branch protection"), "{message}");
    assert!(message.contains("stale merge ref"), "{message}");
    assert!(
        message.contains("commits behind base 'agent-main'"),
        "{message}"
    );
    assert_eq!(host.task_status("T1"), TaskStatus::Review);
}

/// [AC3] The same refusal over a candidate that is current with its base says
/// the opposite, so the two situations are never confused for each other.
#[test]
fn blocked_checks_on_a_current_candidate_are_reported_as_a_real_failure() {
    let workspace = pr_workspace();
    let published_head_sha = git(&workspace.repo, &["rev-parse", "HEAD"]);
    let host = pinned_host(&workspace);
    host.queue_pr_status([open_state("BLOCKED")]);
    let mut input = pinned_input(&workspace);
    input["published_head_sha"] = json!(published_head_sha);

    let error = pr_complete(&host, &input).expect_err("blocked checks must fail the run");

    let message = error.to_string();
    assert!(message.contains("branch protection"), "{message}");
    assert!(
        message.contains("current with base 'agent-main'"),
        "{message}"
    );
    assert!(!message.contains("stale merge ref"), "{message}");
    assert_eq!(host.task_status("T1"), TaskStatus::Review);
}

fn pinned_host(workspace: &PrWorkspace) -> PrOpenTestHost {
    PrOpenTestHost::new(
        vec![review_batch_task("T1", None, None)],
        workspace.repo.clone(),
    )
}

/// A completion input carrying the candidate checkpoints the PR pipeline pins
/// for `complete_pr`.
fn pinned_input(workspace: &PrWorkspace) -> Value {
    json!({
        "job_run_id": "batch-1",
        "completed_task_ids": ["T1"],
        "workspace_path": workspace.repo.to_string_lossy(),
        "pr_number": "42",
        "completion": "done",
        "head": "orbit/test-batch",
        "published_head_sha": PUBLISHED_HEAD_SHA,
        "base": "agent-main",
        "base_sync": "remote",
        "poll_interval_seconds": 0,
        "max_wait_seconds": 0,
    })
}

fn merged_state(head_sha: &str) -> Value {
    json!({
        "number": 42,
        "state": "MERGED",
        "mergedAt": "2026-09-09T01:23:08Z",
        "headRefName": "orbit/test-batch",
        "baseRefName": "agent-main",
        "headRefOid": head_sha,
        "mergeCommit": { "oid": LANDED_COMMIT_SHA },
    })
}

fn open_state(merge_state_status: &str) -> Value {
    json!({
        "number": 42,
        "state": "OPEN",
        "mergedAt": Value::Null,
        "mergeStateStatus": merge_state_status,
        "headRefName": "orbit/test-batch",
        "baseRefName": "agent-main",
        "headRefOid": PUBLISHED_HEAD_SHA,
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
