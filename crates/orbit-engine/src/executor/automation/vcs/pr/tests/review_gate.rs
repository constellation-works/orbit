//! PR steps recheck the reviewed candidate [ORB-11333].

use std::fs;
use std::path::Path;

use orbit_types::task::TaskStatus;
use serde_json::{Value, json};

use super::super::complete::pr_complete;
use super::super::open::pr_open;
use super::test_support::{
    PR_CREATE_OPERATION, PR_MERGE_OPERATION, PUSH_OPERATION, PrOpenTestHost, batch_task, git,
    pr_open_input, pr_workspace, review_batch_task,
};
use crate::executor::automation::vcs::failure::pr_failure_handoff;
use crate::executor::automation::vcs::push::push_batch_changes;

const SUMMARY: &str = "Outcome: success\n\nChanges:\n- Reviewed change.";

#[test]
fn pr_open_refuses_a_head_or_base_that_differs_from_the_reviewed_candidate() {
    let workspace = pr_workspace();
    let host = PrOpenTestHost::new(
        vec![batch_task("T1", "Reviewed task", SUMMARY)],
        workspace.repo.clone(),
    );
    let head = git(&workspace.repo, &["rev-parse", "HEAD"]);

    let mut stale_head = pr_open_input(&workspace.repo, vec!["T1"]);
    stale_head["reviewed_head_sha"] = json!("0123456789abcdef0123456789abcdef01234567");
    let error = pr_open(&host, &stale_head).expect_err("moved head");
    assert!(error.to_string().contains("review_gate_stale"), "{error}");
    assert!(
        host.vcs_calls()
            .iter()
            .all(|call| call.operation != PR_CREATE_OPERATION),
        "no PR is created for an unreviewed head"
    );
    assert!(
        host.comments_for("T1")
            .last()
            .is_some_and(|comment| comment.message.contains("[phase=stale-review-gate]"))
    );

    let mut stale_base = pr_open_input(&workspace.repo, vec!["T1"]);
    stale_base["reviewed_head_sha"] = json!(head);
    stale_base["reviewed_base_sha"] = json!("fedcba9876543210fedcba9876543210fedcba98");
    let error = pr_open(&host, &stale_base).expect_err("moved base");
    assert!(
        error.to_string().contains("not the reviewed base"),
        "{error}"
    );

    let mut reviewed = pr_open_input(&workspace.repo, vec!["T1"]);
    reviewed["reviewed_head_sha"] = json!(head);
    reviewed["reviewed_base_sha"] = reviewed["base_sha"].clone();
    let output = pr_open(&host, &reviewed).expect("reviewed candidate opens");
    assert_eq!(output["pr_created"], true);

    // An empty pin means no gate applied to this run.
    let mut ungated = pr_open_input(&workspace.repo, vec!["T1"]);
    ungated["reviewed_head_sha"] = json!("");
    pr_open(&host, &ungated).expect("ungated reuse");
}

fn status(merge_state: &str, head_oid: &str) -> Value {
    json!({
        "number": 42,
        "state": "OPEN",
        "mergedAt": Value::Null,
        "mergeStateStatus": merge_state,
        "headRefName": "orbit/test-batch",
        "headRefOid": head_oid,
        "baseRefName": "agent-main",
    })
}

fn merged(head_oid: &str, merge_commit: &str) -> Value {
    json!({
        "number": 42,
        "state": "MERGED",
        "mergedAt": "2026-09-07T00:00:00Z",
        "headRefOid": head_oid,
        "mergeCommit": { "oid": merge_commit },
    })
}

fn complete_input(workspace: &std::path::Path, reviewed_head: &str) -> Value {
    json!({
        "job_run_id": "batch-1",
        "completed_task_ids": ["T1"],
        "workspace_path": workspace.to_string_lossy(),
        "pr_number": "42",
        "base": "agent-main",
        "poll_interval_seconds": 0,
        "max_wait_seconds": 0,
        "reviewed_head_sha": reviewed_head,
    })
}

#[test]
fn managed_completion_pins_the_reviewed_head_and_records_the_landing() {
    let workspace = pr_workspace();
    let host = PrOpenTestHost::new(
        vec![review_batch_task("T1", None, None)],
        workspace.repo.clone(),
    );
    let reviewed = "1111111111111111111111111111111111111111";

    // A PR whose head moved after the gate is not merged.
    host.queue_pr_status([status("CLEAN", "2222222222222222222222222222222222222222")]);
    let error =
        pr_complete(&host, &complete_input(&workspace.repo, reviewed)).expect_err("moved head");
    assert!(error.to_string().contains("review_gate_stale"), "{error}");
    assert!(
        host.vcs_calls()
            .iter()
            .all(|call| call.operation != PR_MERGE_OPERATION),
        "no merge is requested for an unreviewed head"
    );
    assert_eq!(host.task_status("T1"), TaskStatus::Review);

    // A conflicting reviewed PR is never repaired into unreviewed content.
    host.queue_pr_status([status("DIRTY", reviewed)]);
    let error = pr_complete(&host, &complete_input(&workspace.repo, reviewed))
        .expect_err("conflict under a gate");
    assert!(
        error.to_string().contains("needs a fresh review"),
        "{error}"
    );

    // The reviewed head merges, and completion reports the landing for
    // verification against the certificate.
    host.queue_pr_status([
        status("CLEAN", reviewed),
        merged(reviewed, "3333333333333333333333333333333333333333"),
    ]);
    host.queue_merge_capabilities(true, true, true, false);
    host.queue_vcs_result(
        PR_MERGE_OPERATION,
        json!({
            "landed_commit": "3333333333333333333333333333333333333333"
        }),
    );
    let output = pr_complete(&host, &complete_input(&workspace.repo, reviewed))
        .expect("reviewed head merges");
    assert_eq!(output["merge"]["merged"], true);
    assert_eq!(
        output["merge"]["landed_commit"],
        "3333333333333333333333333333333333333333"
    );
    let landings = host.review_landings();
    assert_eq!(landings.len(), 1);
    assert_eq!(landings[0].reviewed_head_sha, reviewed);
    assert!(landings[0].managed_merge);
    let merge_call = host
        .vcs_calls()
        .into_iter()
        .find(|call| call.operation == PR_MERGE_OPERATION)
        .expect("merge call");
    assert_eq!(merge_call.input["reviewed_head_sha"], reviewed);
    assert_eq!(merge_call.input["auto"], false);
    assert_eq!(
        landings[0].landed_commit.as_deref(),
        Some("3333333333333333333333333333333333333333")
    );
    assert_eq!(landings[0].pr_number, "42");
    assert_eq!(host.task_status("T1"), TaskStatus::Done);
}

#[test]
fn gated_pending_checks_wait_locally_without_enabling_auto_merge() {
    let workspace = pr_workspace();
    let reviewed = "1111111111111111111111111111111111111111";
    let host = PrOpenTestHost::new(
        vec![review_batch_task("T1", None, None)],
        workspace.repo.clone(),
    );
    host.queue_pr_status([
        status("PENDING", reviewed),
        status("CLEAN", reviewed),
        merged(reviewed, "landed"),
    ]);
    host.queue_vcs_result(PR_MERGE_OPERATION, json!({"landed_commit": "landed"}));
    let mut input = complete_input(&workspace.repo, reviewed);
    input["max_wait_seconds"] = json!(10);
    let output = pr_complete(&host, &input).expect("wait then merge reviewed candidate");
    assert_eq!(output["merge"]["auto_merge_requested"], false);
    let merges: Vec<_> = host
        .vcs_calls()
        .into_iter()
        .filter(|call| call.operation == PR_MERGE_OPERATION)
        .collect();
    assert_eq!(merges.len(), 1);
    assert_eq!(merges[0].input["auto"], false);
    assert_eq!(merges[0].input["reviewed_head_sha"], reviewed);
}

#[test]
fn gated_pending_timeout_leaves_no_deferred_merge_and_external_landing_is_distinct() {
    let workspace = pr_workspace();
    let reviewed = "1111111111111111111111111111111111111111";
    let host = PrOpenTestHost::new(
        vec![review_batch_task("T1", None, None)],
        workspace.repo.clone(),
    );
    host.queue_pr_status([status("PENDING", reviewed)]);
    let error = pr_complete(&host, &complete_input(&workspace.repo, reviewed))
        .expect_err("pending timeout");
    assert!(error.to_string().contains("timed out"));
    assert!(
        host.vcs_calls()
            .iter()
            .all(|call| call.operation != PR_MERGE_OPERATION)
    );
    assert_eq!(host.task_status("T1"), TaskStatus::Review);

    host.queue_pr_status([merged("external-head", "external-merge")]);
    let output = pr_complete(&host, &complete_input(&workspace.repo, reviewed))
        .expect("observe external merge");
    assert_eq!(output["merge"]["managed_merge"], false);
    let landings = host.review_landings();
    assert!(!landings[0].managed_merge);
    assert_eq!(landings[0].landed_commit.as_deref(), Some("external-merge"));
    assert!(
        host.vcs_calls()
            .iter()
            .all(|call| call.operation != PR_MERGE_OPERATION)
    );
}

#[cfg(unix)]
#[test]
fn managed_completion_rejects_a_push_between_status_and_provider_mutation() {
    use super::super::super::tests::with_fake_gh;

    // The provider returns A in the first status response, then advances its
    // own head to B before accepting any merge mutation. An unconditioned
    // command really merges B, so this test fails against the original code.
    let script = r#"#!/bin/sh
set -eu
printf '%s\n' "$@" >> provider-args
if [ "$1 $2" = "pr view" ]; then
    if [ -f provider-merged ]; then
        printf '%s\n' '{"state":"MERGED","headRefOid":"2222222222222222222222222222222222222222","mergeCommit":{"oid":"unreviewed-merge"}}'
    else
        printf '%s\n' '{"state":"OPEN","mergeStateStatus":"CLEAN","headRefOid":"1111111111111111111111111111111111111111"}'
        printf '%s' '1111111111111111111111111111111111111111' > provider-head
    fi
    exit 0
fi
# This command only starts after Orbit has inspected the status response.
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
printf '%s\n' '{"merged":true,"sha":"unreviewed-merge"}'
"#;
    if !with_fake_gh(
        module_path!(),
        "managed_completion_rejects_a_push_between_status_and_provider_mutation",
        script,
    ) {
        return;
    }
    let workspace = pr_workspace();
    let host = PrOpenTestHost::new(
        vec![review_batch_task("T1", None, None)],
        workspace.repo.clone(),
    )
    .with_provider_completion();
    let reviewed = "1111111111111111111111111111111111111111";
    let error = pr_complete(&host, &complete_input(&workspace.repo, reviewed))
        .expect_err("provider must reject changed head");
    assert!(error.to_string().contains("HTTP 409"), "{error}");
    assert_eq!(
        std::fs::read_to_string(workspace.repo.join("provider-head"))
            .expect("advanced provider head"),
        "2222222222222222222222222222222222222222"
    );
    assert!(!workspace.repo.join("provider-merged").exists());
    assert_eq!(host.task_status("T1"), TaskStatus::Review);
    assert!(host.review_landings().is_empty());
    let args = std::fs::read_to_string(workspace.repo.join("provider-args"))
        .expect("actual provider args");
    let expected_mutation = format!(
        "api\nrepos/{{owner}}/{{repo}}/pulls/42/merge\n--method\nPUT\n-f\nsha={reviewed}\n-f\nmerge_method=squash\n"
    );
    assert!(args.contains(&expected_mutation), "{args}");
}

/// [ORB-11538] A rewritten candidate whose origin SHA is not an ancestor must
/// still be preserved: checkpoint-less push stays fail-closed, and the
/// review-gate handoff supplies the durable rewrite lease so the blocked
/// escalation can run.
#[test]
fn review_gate_handoff_pushes_a_diverged_candidate_with_a_rewrite_lease() {
    let workspace = pr_workspace();
    let (head_before, published) = diverge_published_candidate(&workspace.repo);
    let task_id = "ORB-11538-DIVERGED";
    let host = PrOpenTestHost::new(
        vec![batch_task(
            task_id,
            "Preserve rewritten review-gate candidate",
            SUMMARY,
        )],
        workspace.repo.clone(),
    );

    let error = push_batch_changes(
        &host,
        &json!({
            "workspace_path": workspace.repo,
            "branch": "orbit/test-batch",
        }),
    )
    .expect_err("checkpoint-less push must not replace a diverged origin");
    assert!(
        error.to_string().contains("no durable rewrite checkpoint"),
        "{error}"
    );
    assert!(host.vcs_calls().is_empty(), "no push without a lease");
    assert_eq!(host.task_status(task_id), TaskStatus::InProgress);

    let recovered = pr_failure_handoff(
        &host,
        &review_gate_handoff_input(
            &workspace.repo,
            task_id,
            json!({
                "head": "orbit/test-batch",
                "rewritten": true,
                "head_sha_before": head_before,
                "remote_sha_before": published,
            }),
        ),
    )
    .expect("lease-checked preservation must reach the blocked update");

    assert_eq!(recovered["decision"], "blocked_review_gate");
    assert_eq!(recovered["pr_created"], false);
    assert_eq!(recovered["task_status"], "blocked");
    assert_eq!(recovered["push"]["decision"], "performed_force_with_lease");
    assert_eq!(recovered["push"]["force_with_lease"], true);
    assert_eq!(host.task_status(task_id), TaskStatus::Blocked);
    let push = host
        .vcs_calls()
        .into_iter()
        .find(|call| call.operation == PUSH_OPERATION)
        .expect("preservation push");
    assert_eq!(push.input["force_with_lease"], true);
    assert_eq!(push.input["expected_remote_sha"], json!(published));
    assert!(
        host.vcs_calls()
            .iter()
            .all(|call| call.operation != PR_CREATE_OPERATION),
        "review-gate preservation must not open a PR"
    );
    let update = host
        .automation_updates()
        .into_iter()
        .find(|(_, update)| update.status == Some(TaskStatus::Blocked))
        .expect("blocked review-gate update");
    assert_eq!(
        update.1.status_event.as_deref(),
        Some("review_gate_escalation")
    );
}

#[test]
fn review_gate_handoff_creates_a_missing_origin_branch_and_fast_forwards_without_force() {
    let missing = pr_workspace();
    git(
        &missing.repo,
        &["push", "origin", "--delete", "orbit/test-batch"],
    );
    let missing_id = "ORB-11538-CREATE";
    let missing_host = PrOpenTestHost::new(
        vec![batch_task(
            missing_id,
            "Create review-gate candidate",
            SUMMARY,
        )],
        missing.repo.clone(),
    );
    let created = pr_failure_handoff(
        &missing_host,
        &review_gate_handoff_input(
            &missing.repo,
            missing_id,
            json!({
                "head": "orbit/test-batch",
                "rewritten": false,
                "head_sha_before": git(&missing.repo, &["rev-parse", "HEAD"]),
                "remote_sha_before": Value::Null,
            }),
        ),
    )
    .expect("first-time preservation creates the branch");
    assert_eq!(created["decision"], "blocked_review_gate");
    assert_eq!(created["push"]["decision"], "performed_create");
    assert_eq!(created["push"]["force_with_lease"], false);
    assert_eq!(missing_host.vcs_calls()[0].input["force_with_lease"], false);
    assert_eq!(missing_host.task_status(missing_id), TaskStatus::Blocked);

    let fast_forward = pr_workspace();
    fs::write(fast_forward.repo.join("fast-forward.txt"), "local\n").expect("write follow-up");
    git(&fast_forward.repo, &["add", "fast-forward.txt"]);
    git(&fast_forward.repo, &["commit", "-m", "local follow-up"]);
    let ff_id = "ORB-11538-FF";
    let ff_host = PrOpenTestHost::new(
        vec![batch_task(
            ff_id,
            "Fast-forward review-gate candidate",
            SUMMARY,
        )],
        fast_forward.repo.clone(),
    );
    let origin_sha = git(
        &fast_forward.repo,
        &["rev-parse", "origin/orbit/test-batch"],
    );
    let forwarded = pr_failure_handoff(
        &ff_host,
        &review_gate_handoff_input(
            &fast_forward.repo,
            ff_id,
            json!({
                "head": "orbit/test-batch",
                "rewritten": false,
                "head_sha_before": git(&fast_forward.repo, &["rev-parse", "HEAD"]),
                "remote_sha_before": origin_sha,
            }),
        ),
    )
    .expect("fast-forward preservation must not force-push");
    assert_eq!(forwarded["decision"], "blocked_review_gate");
    assert_eq!(forwarded["push"]["decision"], "performed_fast_forward");
    assert_eq!(forwarded["push"]["force_with_lease"], false);
    assert_eq!(ff_host.vcs_calls()[0].input["force_with_lease"], false);
    assert_eq!(ff_host.task_status(ff_id), TaskStatus::Blocked);
}

fn review_gate_handoff_input(repo: &Path, task_id: &str, sync_base: Value) -> Value {
    json!({
        "failed_step_id": "review_gate_settle",
        "activity_name": "review_gate_settle",
        "error_code": "review_gate_failed",
        "error_message": "before-PR review gate did not pass",
        "run_id": "batch-1",
        "job_input": {
            "task_ids": [task_id],
            "base_branch": "agent-main",
            "base_sync": "local",
        },
        "pipeline": {
            "worktree": {
                "workspace_path": repo,
                "job_run_id": "batch-1",
                "base_ref": "agent-main",
            },
            "sync_base": sync_base,
        },
    })
}

fn diverge_published_candidate(repo: &Path) -> (String, String) {
    let head_before = git(repo, &["rev-parse", "HEAD"]);
    fs::write(repo.join("published-side.txt"), "published\n").expect("write published side");
    git(repo, &["add", "published-side.txt"]);
    git(repo, &["commit", "-m", "previous preservation candidate"]);
    git(repo, &["push", "origin", "orbit/test-batch"]);
    let published = git(repo, &["rev-parse", "HEAD"]);
    git(repo, &["reset", "--hard", &head_before]);
    fs::write(repo.join("retry-side.txt"), "retry\n").expect("write retry side");
    git(repo, &["add", "retry-side.txt"]);
    git(repo, &["commit", "-m", "rewritten review-gate candidate"]);
    (head_before, published)
}
