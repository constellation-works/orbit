use std::fs;
use std::path::Path;

use orbit_types::task::{Task, TaskArtifact, TaskStatus};
use orbit_types::workflow::PipelineState;
use serde_json::{Value, json};

use crate::context::RuntimeHost;

use super::super::super::git::{git_output, git_success};
use super::super::super::pr::{pr_complete, pr_promote};
use super::super::git_commit;
use super::test_support::{CommitTestHost, initialized_git_repo, task_with_file};

fn fixture() -> (tempfile::TempDir, Task, Value, Value, Value) {
    coverage_fixture("T1", "T2")
}

fn sibling_fixture() -> (tempfile::TempDir, Task, Value, Value, Value) {
    coverage_fixture("T2", "T3")
}

fn coverage_fixture(
    covering_task_id: &str,
    later_task_id: &str,
) -> (tempfile::TempDir, Task, Value, Value, Value) {
    let temp = initialized_git_repo();
    let workspace = temp.path();
    let base = git_output(workspace, &["rev-parse", "HEAD"]).unwrap();
    git_success(workspace, &["checkout", "-b", "candidate"]).unwrap();
    fs::write(workspace.join("task.txt"), "delivered behavior\n").unwrap();
    git_success(workspace, &["add", "task.txt"]).unwrap();
    git_success(
        workspace,
        &[
            "commit",
            "-m",
            &format!("Fix behavior [{covering_task_id}]"),
        ],
    )
    .unwrap();
    git_success(workspace, &["checkout", "-b", "integration", &base]).unwrap();
    git_success(
        workspace,
        &[
            "merge",
            "--no-ff",
            "candidate",
            "-m",
            &format!("Merge fix [{covering_task_id}]"),
        ],
    )
    .unwrap();
    let covering = git_output(workspace, &["rev-parse", "HEAD"]).unwrap();
    // The already-landed commit is older than the current tested base, as in
    // the real retry. Unrelated intervening changes do not invalidate it.
    fs::write(workspace.join("other.txt"), "later unrelated work\n").unwrap();
    git_success(workspace, &["add", "other.txt"]).unwrap();
    git_success(
        workspace,
        &["commit", "-m", &format!("Other task [{later_task_id}]")],
    )
    .unwrap();
    let head = git_output(workspace, &["rev-parse", "HEAD"]).unwrap();
    let mut task = task_with_file("T1", "Fix behavior", "task.txt", "codex");
    task.acceptance_criteria = vec!["Behavior works".to_string()];
    let report = json!({
        "schema_version": 1, "task_id": "T1", "run_id": "batch-1",
        "tested_head": head, "covering_commit": covering,
        "covering_task_id": covering_task_id,
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

fn assert_git_unmutated(workspace: &Path, head: &Value) {
    assert_eq!(
        json!(git_output(workspace, &["rev-parse", "HEAD"]).unwrap()),
        *head
    );
    assert!(
        git_output(workspace, &["status", "--porcelain"])
            .unwrap()
            .is_empty()
    );
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
    assert_git_unmutated(temp.path(), &input["base_sha"]);
}

#[test]
fn stale_no_diff_evidence_does_not_shadow_verified_already_landed() {
    let (temp, task, input, report, log) = fixture();
    let stale = json!({
        "schema_version": 1, "task_id": "T1", "run_id": "earlier-run",
        "tested_head": input["base_sha"], "reason": "earlier attempt",
        "validation": [{"command": "test behavior", "exit_code": 0,
            "log_artifact": "validation.json"}],
    });
    let mut stored = artifacts(&report, &log);
    stored.push(TaskArtifact::from_text("no-diff.json", stale.to_string()));
    let host = CommitTestHost::new(vec![task], temp.path().to_path_buf()).with_artifacts(stored);
    let checkpoint = git_commit(&host, &input).expect("already-landed evidence still verifies");
    assert_eq!(checkpoint["decision"], "verified_already_landed");
    assert_git_unmutated(temp.path(), &input["base_sha"]);
}

#[test]
fn already_landed_sibling_coverage_completes_without_new_commit() {
    let (temp, task, input, report, log) = sibling_fixture();
    let host = CommitTestHost::new(vec![task], temp.path().to_path_buf())
        .with_artifacts(artifacts(&report, &log));
    let checkpoint =
        git_commit(&host, &input).expect("sibling already-landed revalidation succeeds");
    assert_eq!(checkpoint["decision"], "verified_already_landed");
    assert_eq!(checkpoint["committed"], false);
    assert!(checkpoint.get("commit_sha").is_none());
    assert_eq!(checkpoint["already_landed"], report);
    assert_eq!(checkpoint["already_landed"]["task_id"], "T1");
    assert_eq!(checkpoint["already_landed"]["covering_task_id"], "T2");
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
    assert_git_unmutated(temp.path(), &input["base_sha"]);
}

#[test]
fn already_landed_same_task_still_requires_current_task_covering_marker() {
    let (temp, task, input, mut report, log) = sibling_fixture();
    report["covering_task_id"] = json!("T1");
    let host = CommitTestHost::new(vec![task], temp.path().to_path_buf())
        .with_artifacts(artifacts(&report, &log));
    let error = git_commit(&host, &input).unwrap_err();
    assert!(
        error.to_string().contains("already_landed_unverified"),
        "{error}"
    );
    assert!(
        error
            .to_string()
            .contains("no matching task delivery marker"),
        "{error}"
    );
    assert_eq!(host.get_task("T1").unwrap().status, TaskStatus::InProgress);
    assert_git_unmutated(temp.path(), &input["base_sha"]);
}

#[test]
fn already_landed_refuses_missing_or_invalid_evidence_without_mutating_git() {
    for fault in [
        "missing",
        "unrelated",
        "unrelated_sibling",
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
        "empty_covering_task",
        "missing_required",
        "failed_log",
        "wrong_schema",
        "unknown_field",
    ] {
        let (temp, mut task, input, mut report, mut log) = fixture();
        match fault {
            "unrelated" => report["covering_commit"] = input["base_sha"].clone(),
            "unrelated_sibling" => {
                report["covering_task_id"] = json!("T2");
                report["covering_commit"] = input["base_sha"].clone();
            }
            "scope" => task.acceptance_criteria.push("New requirement".to_string()),
            "run" => report["run_id"] = json!("different-run"),
            "head" => report["tested_head"] = report["covering_commit"].clone(),
            "missing_check" => report["validation"] = json!([]),
            "failed" | "denied" | "not_run" => report["validation"][0]["outcome"] = json!(fault),
            "stale_log" => log["tested_head"] = report["covering_commit"].clone(),
            "missing_criterion" => report["criteria_evidence"] = json!([]),
            "cross_task" => report["covering_task_id"] = json!("T2"),
            "empty_covering_task" => report["covering_task_id"] = json!(""),
            "missing_required" => {
                report["required_commands"] = json!(["test behavior", "workspace lint"])
            }
            "failed_log" => log["exit_code"] = json!(1),
            "wrong_schema" => report["schema_version"] = json!(2),
            "unknown_field" => report["unexpected"] = json!(true),
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
fn already_landed_refuses_nested_validation_wrapper_with_flattened_shape_hint() {
    let (temp, task, input, mut report, log) = fixture();
    report["validation"] = json!([{
        "validation": {
            "command": "test behavior",
            "outcome": "passed",
            "role": "required"
        },
        "log_artifact": "validation.json"
    }]);
    let host = CommitTestHost::new(vec![task], temp.path().to_path_buf())
        .with_artifacts(artifacts(&report, &log));

    let error = git_commit(&host, &input).expect_err("nested validation wrapper");
    let message = error.to_string();
    assert!(message.contains("already_landed_unverified"), "{message}");
    assert!(message.contains("flatten"), "{message}");
    for field in ["command", "outcome", "role", "log_artifact"] {
        assert!(message.contains(field), "missing {field} in: {message}");
    }
    assert_eq!(host.get_task("T1").unwrap().status, TaskStatus::InProgress);
    assert_git_unmutated(temp.path(), &input["base_sha"]);
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

#[test]
fn already_landed_refuses_unrelated_sibling_covering_commit() {
    let (temp, task, input, mut report, log) = fixture();
    report["covering_task_id"] = json!("T2");
    report["covering_commit"] = input["base_sha"].clone();
    let host = CommitTestHost::new(vec![task], temp.path().to_path_buf())
        .with_artifacts(artifacts(&report, &log));
    let error = git_commit(&host, &input).unwrap_err();
    assert!(
        error.to_string().contains("already_landed_unverified"),
        "{error}"
    );
    assert!(
        error
            .to_string()
            .contains("did not change the task's declared scope"),
        "{error}"
    );
    assert_git_unmutated(temp.path(), &input["base_sha"]);
}

#[test]
fn already_landed_refuses_dirty_tree_without_mutating_git() {
    let (temp, task, input, report, log) = sibling_fixture();
    fs::write(temp.path().join("dirty.txt"), "pending work").unwrap();
    let host = CommitTestHost::new(vec![task], temp.path().to_path_buf())
        .with_artifacts(artifacts(&report, &log));
    let error = git_commit(&host, &input).unwrap_err();
    assert!(
        error.to_string().contains("unknown untracked paths"),
        "{error}"
    );
    assert_eq!(host.get_task("T1").unwrap().status, TaskStatus::InProgress);
    assert_eq!(
        json!(git_output(temp.path(), &["rev-parse", "HEAD"]).unwrap()),
        input["base_sha"]
    );
}

#[test]
fn already_landed_refuses_covering_commit_that_is_not_ancestor_of_head() {
    let temp = initialized_git_repo();
    let workspace = temp.path();
    fs::write(workspace.join("task.txt"), "old behavior\n").unwrap();
    git_success(workspace, &["add", "task.txt"]).unwrap();
    git_success(workspace, &["commit", "-m", "seed task.txt"]).unwrap();
    let fork = git_output(workspace, &["rev-parse", "HEAD"]).unwrap();
    git_success(workspace, &["checkout", "-b", "repair"]).unwrap();
    fs::write(workspace.join("task.txt"), "delivered behavior\n").unwrap();
    git_success(workspace, &["add", "task.txt"]).unwrap();
    git_success(workspace, &["commit", "-m", "Fix behavior [T2]"]).unwrap();
    let covering = git_output(workspace, &["rev-parse", "HEAD"]).unwrap();
    git_success(workspace, &["checkout", "-B", "integration", &fork]).unwrap();
    fs::write(workspace.join("other.txt"), "unrelated\n").unwrap();
    git_success(workspace, &["add", "other.txt"]).unwrap();
    git_success(workspace, &["commit", "-m", "Other task [T3]"]).unwrap();
    let head = git_output(workspace, &["rev-parse", "HEAD"]).unwrap();
    let mut task = task_with_file("T1", "Fix behavior", "task.txt", "codex");
    task.acceptance_criteria = vec!["Behavior works".to_string()];
    let report = json!({
        "schema_version": 1, "task_id": "T1", "run_id": "batch-1",
        "tested_head": head, "covering_commit": covering, "covering_task_id": "T2",
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
    let host = CommitTestHost::new(vec![task], workspace.to_path_buf())
        .with_artifacts(artifacts(&report, &log));
    let error = git_commit(&host, &input).unwrap_err();
    assert!(
        error.to_string().contains("already_landed_unverified"),
        "{error}"
    );
    assert!(
        error
            .to_string()
            .contains("not a verified ancestor of tested HEAD"),
        "{error}"
    );
    assert_eq!(host.get_task("T1").unwrap().status, TaskStatus::InProgress);
    assert_git_unmutated(workspace, &input["base_sha"]);
}
