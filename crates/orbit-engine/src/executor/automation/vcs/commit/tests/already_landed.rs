use std::fs;

use orbit_types::task::{Task, TaskArtifact, TaskStatus};
use orbit_types::workflow::PipelineState;
use serde_json::{Value, json};

use crate::context::RuntimeHost;

use super::super::super::git::{git_output, git_success};
use super::super::super::pr::{pr_complete, pr_promote};
use super::super::git_commit;
use super::test_support::{CommitTestHost, initialized_git_repo, task_with_file};

fn fixture() -> (tempfile::TempDir, Task, Value, Value, Value) {
    let temp = initialized_git_repo();
    let workspace = temp.path();
    let base = git_output(workspace, &["rev-parse", "HEAD"]).unwrap();
    git_success(workspace, &["checkout", "-b", "candidate"]).unwrap();
    fs::write(workspace.join("task.txt"), "delivered behavior\n").unwrap();
    git_success(workspace, &["add", "task.txt"]).unwrap();
    git_success(workspace, &["commit", "-m", "Fix behavior [T1]"]).unwrap();
    git_success(workspace, &["checkout", "-b", "integration", &base]).unwrap();
    git_success(
        workspace,
        &["merge", "--no-ff", "candidate", "-m", "Merge fix [T1]"],
    )
    .unwrap();
    let covering = git_output(workspace, &["rev-parse", "HEAD"]).unwrap();
    // The already-landed commit is older than the current tested base, as in
    // the real retry. Unrelated intervening changes do not invalidate it.
    fs::write(workspace.join("other.txt"), "later unrelated work\n").unwrap();
    git_success(workspace, &["add", "other.txt"]).unwrap();
    git_success(workspace, &["commit", "-m", "Other task [T2]"]).unwrap();
    let head = git_output(workspace, &["rev-parse", "HEAD"]).unwrap();
    let mut task = task_with_file("T1", "Fix behavior", "task.txt", "codex");
    task.acceptance_criteria = vec!["Behavior works".to_string()];
    let report = json!({
        "schema_version": 1, "task_id": "T1", "run_id": "batch-1",
        "tested_head": head, "covering_commit": covering, "covering_task_id": "T1",
        "scope": {
            "title": task.title, "description": task.description,
            "plan": task.plan, "acceptance_criteria": task.acceptance_criteria,
            "context_files": task.context_files,
            "tags": task.tags, "relations": task.relations,
            "required_tools": task.required_tools, "type": task.task_type,
            "comments": [],
        },
        "required_commands": ["test behavior"],
        "criteria_evidence": ["The behavior regression passed on current HEAD"],
        "validation": [{"command": "test behavior", "outcome": "passed",
            "role": "required", "log_artifact": "validation.json"}],
    });
    let log = json!({"run_id": "batch-1", "tested_head": head,
        "command": "test behavior", "exit_code": 0, "output": "1 test passed"});
    let input = json!({"scope": "all", "job_run_id": "batch-1", "verify_already_landed": true,
        "workspace_path": workspace, "base_sha": head});
    (temp, task, input, report, log)
}

fn artifacts(report: &Value, log: &Value) -> Vec<TaskArtifact> {
    vec![
        TaskArtifact::from_text("already-landed.json", report.to_string()),
        TaskArtifact::from_text("validation.json", log.to_string()),
    ]
}

#[test]
fn already_landed_retry_completes_without_new_commit_or_pr_and_is_idempotent() {
    let (temp, task, input, report, log) = fixture();
    let host = CommitTestHost::new(vec![task], temp.path().to_path_buf())
        .with_artifacts(artifacts(&report, &log));
    let mut legacy_input = input.clone();
    legacy_input["verify_already_landed"] = json!(false);
    assert!(
        git_commit(&host, &legacy_input)
            .unwrap_err()
            .to_string()
            .contains("nothing to commit"),
        "the same already-merged candidate reproduces the original commit-gate failure"
    );
    let checkpoint = git_commit(&host, &input).expect("already-landed revalidation succeeds");
    assert_eq!(checkpoint["decision"], "verified_already_landed");
    assert_eq!(checkpoint["committed"], false);
    assert!(checkpoint.get("commit_sha").is_none());
    assert_eq!(checkpoint["already_landed"], report);
    assert_eq!(checkpoint["validation_provenance"], json!([log]));
    assert_eq!(git_commit(&host, &input).unwrap(), checkpoint);

    let mut handoff = input.clone();
    handoff["completed_task_ids"] = json!(["T1"]);
    handoff["no_diff_expected"] = json!(true);
    handoff["already_landed_checkpoint"] = checkpoint;
    pr_promote(&host, &handoff).unwrap();
    assert_eq!(host.get_task("T1").unwrap().status, TaskStatus::Review);
    let completed = pr_complete(&host, &handoff).unwrap();
    assert_eq!(completed["merge"]["reason"], "verified_already_landed");
    assert_eq!(completed["merge"]["merged"], false);
    assert_eq!(host.get_task("T1").unwrap().status, TaskStatus::Done);
    assert!(host.get_task("T1").unwrap().external_refs.is_empty());
    assert_eq!(
        pr_complete(&host, &handoff).unwrap()["skipped_task_ids"],
        json!(["T1"])
    );
    assert_eq!(
        git_output(temp.path(), &["rev-parse", "HEAD"]).unwrap(),
        input["base_sha"]
    );
    assert!(
        git_output(temp.path(), &["status", "--porcelain"])
            .unwrap()
            .is_empty()
    );
}

#[test]
fn already_landed_refuses_missing_or_invalid_evidence_without_mutating_git() {
    for fault in [
        "missing",
        "unrelated",
        "scope",
        "run",
        "head",
        "missing_check",
        "failed",
        "denied",
        "not_run",
        "missing_log",
        "stale_log",
        "missing_criterion",
        "cross_task",
        "missing_required",
        "failed_log",
    ] {
        let (temp, mut task, input, mut report, mut log) = fixture();
        match fault {
            "unrelated" => report["covering_commit"] = input["base_sha"].clone(),
            "scope" => task.acceptance_criteria.push("New requirement".to_string()),
            "run" => report["run_id"] = json!("different-run"),
            "head" => report["tested_head"] = report["covering_commit"].clone(),
            "missing_check" => report["validation"] = json!([]),
            "failed" | "denied" | "not_run" => report["validation"][0]["outcome"] = json!(fault),
            "stale_log" => log["tested_head"] = report["covering_commit"].clone(),
            "missing_criterion" => report["criteria_evidence"] = json!([]),
            "cross_task" => report["covering_task_id"] = json!("T2"),
            "missing_required" => {
                report["required_commands"] = json!(["test behavior", "workspace lint"])
            }
            "failed_log" => log["exit_code"] = json!(1),
            _ => {}
        }
        let mut stored = artifacts(&report, &log);
        if fault == "missing" {
            stored.clear();
        }
        if fault == "missing_log" {
            stored.pop();
        }
        let host =
            CommitTestHost::new(vec![task], temp.path().to_path_buf()).with_artifacts(stored);
        let error = git_commit(&host, &input).expect_err(fault);
        assert!(
            error.to_string().contains("already_landed_unverified"),
            "{fault}: {error}"
        );
        assert_eq!(host.get_task("T1").unwrap().status, TaskStatus::InProgress);
        assert_eq!(
            git_output(temp.path(), &["rev-parse", "HEAD"]).unwrap(),
            input["base_sha"],
            "{fault}"
        );
        assert!(
            git_output(temp.path(), &["status", "--porcelain"])
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn already_landed_refuses_changed_implementation_since_covering_commit() {
    let (temp, task, mut input, mut report, mut log) = fixture();
    fs::write(temp.path().join("task.txt"), "changed behavior\n").unwrap();
    git_success(temp.path(), &["add", "task.txt"]).unwrap();
    git_success(temp.path(), &["commit", "-m", "Change behavior"]).unwrap();
    let head = git_output(temp.path(), &["rev-parse", "HEAD"]).unwrap();
    input["base_sha"] = json!(head);
    report["tested_head"] = json!(head);
    log["tested_head"] = json!(head);
    let host = CommitTestHost::new(vec![task], temp.path().to_path_buf())
        .with_artifacts(artifacts(&report, &log));
    assert!(
        git_commit(&host, &input)
            .unwrap_err()
            .to_string()
            .contains("changed since landing")
    );
}

#[test]
fn already_landed_resume_preserves_source_checkpoint_and_validation_provenance() {
    let (temp, task, mut input, report, log) = fixture();
    let mut source = PipelineState::new(
        "batch-1".to_string(),
        "task_pr_pipeline".to_string(),
        json!({"task_ids": ["T1"]}),
    );
    source.pipeline = json!({"implement_bundle": {"historical": "unchanged"}});
    let resumed = PipelineState::new(
        "resume-1".to_string(),
        "task_pr_pipeline".to_string(),
        json!({"task_ids": ["T1"]}),
    );
    let host = CommitTestHost::new(vec![task], temp.path().to_path_buf())
        .with_artifacts(artifacts(&report, &log))
        .with_run_state("batch-1", None, source.clone())
        .with_run_state("resume-1", Some("batch-1"), resumed);
    input["run_id"] = json!("resume-1");
    let checkpoint = git_commit(&host, &input).unwrap();
    assert_eq!(checkpoint["already_landed"]["run_id"], "batch-1");
    assert_eq!(checkpoint["job_run_id"], "resume-1");
    assert_eq!(
        host.read_run_state("batch-1").unwrap().unwrap().pipeline,
        source.pipeline
    );
}

#[test]
fn already_landed_promotion_rechecks_dirty_tree_and_checkpoint_tampering() {
    let (temp, task, mut input, report, log) = fixture();
    let host = CommitTestHost::new(vec![task], temp.path().to_path_buf())
        .with_artifacts(artifacts(&report, &log));
    let checkpoint = git_commit(&host, &input).unwrap();
    input["completed_task_ids"] = json!(["T1"]);
    input["no_diff_expected"] = json!(true);
    input["already_landed_checkpoint"] = checkpoint;
    input["already_landed_checkpoint"]["validation_provenance"][0]["output"] = json!("rewritten");
    assert!(
        pr_promote(&host, &input)
            .unwrap_err()
            .to_string()
            .contains("evidence changed")
    );
    input["already_landed_checkpoint"]["validation_provenance"][0]["output"] =
        log["output"].clone();
    fs::write(temp.path().join("dirty.txt"), "pending work").unwrap();
    assert!(
        pr_promote(&host, &input)
            .unwrap_err()
            .to_string()
            .contains("no longer clean")
    );
    assert_eq!(host.get_task("T1").unwrap().status, TaskStatus::InProgress);
}

#[test]
fn already_landed_completion_refuses_scope_changed_after_promotion() {
    let (temp, task, mut input, report, log) = fixture();
    let host = CommitTestHost::new(vec![task], temp.path().to_path_buf())
        .with_artifacts(artifacts(&report, &log));
    input["already_landed_checkpoint"] = git_commit(&host, &input).unwrap();
    input["completed_task_ids"] = json!(["T1"]);
    input["no_diff_expected"] = json!(true);
    pr_promote(&host, &input).unwrap();
    let mut changed = host.get_task("T1").unwrap();
    changed
        .acceptance_criteria
        .push("New requirement".to_string());
    let host = CommitTestHost::new(vec![changed], temp.path().to_path_buf())
        .with_artifacts(artifacts(&report, &log));
    assert!(
        pr_complete(&host, &input)
            .unwrap_err()
            .to_string()
            .contains("task scope changed")
    );
    assert_eq!(host.get_task("T1").unwrap().status, TaskStatus::Review);
}

#[test]
fn already_landed_accepts_captured_silent_validation_success() {
    let (temp, task, input, report, mut log) = fixture();
    // Formatters and other required checks can legitimately emit no bytes.
    log["output"] = json!("");
    let host = CommitTestHost::new(vec![task], temp.path().to_path_buf())
        .with_artifacts(artifacts(&report, &log));
    let result = git_commit(&host, &input).unwrap();
    assert_eq!(result["validation_provenance"][0]["output"], "");
}
