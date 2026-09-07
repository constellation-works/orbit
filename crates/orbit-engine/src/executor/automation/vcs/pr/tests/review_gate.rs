//! PR steps recheck the reviewed candidate [ORB-11333].

use orbit_types::task::TaskStatus;
use serde_json::{Value, json};

use super::super::complete::pr_complete;
use super::super::open::pr_open;
use super::test_support::{
    PR_CREATE_OPERATION, PR_MERGE_OPERATION, PrOpenTestHost, batch_task, git, pr_open_input,
    pr_workspace, review_batch_task,
};

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
    assert_eq!(
        landings[0].landed_commit.as_deref(),
        Some("3333333333333333333333333333333333333333")
    );
    assert_eq!(landings[0].pr_number, "42");
    assert_eq!(host.task_status("T1"), TaskStatus::Done);
}
