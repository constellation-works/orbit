//! ORB-13145: a run whose implementation correctly changed nothing finishes the
//! commit step with structured `no-diff.json` evidence bound to this task, this
//! run and the pinned HEAD, without inventing a commit. Every neighbouring
//! empty-tree refusal stays fail-closed.

use std::fs;
use std::path::Path;

use orbit_types::task::{NO_DIFF_EXPECTED_TAG, Task, TaskArtifact, TaskStatus};
use serde_json::{Value, json};

use crate::context::RuntimeHost;

use super::super::super::git::{git_output, git_success};
use super::super::super::pr::{pr_complete, pr_promote};
use super::super::git_commit;
use super::test_support::{CommitTestHost, initialized_git_repo, task_with_file};

struct Fixture {
    temp: tempfile::TempDir,
    task: Task,
    input: Value,
    report: Value,
    log: Value,
}

/// A clean worktree at the pinned base, as in ORB-12992: an untagged task with
/// no context selectors whose implementation edited nothing.
fn fixture() -> Fixture {
    let temp = initialized_git_repo();
    let head = git_output(temp.path(), &["rev-parse", "HEAD"]).unwrap();
    let mut task = task_with_file("T1", "Confirm behavior", "README.md", "codex");
    task.context_files.clear();
    let report = json!({
        "schema_version": 1, "task_id": "T1", "run_id": "batch-1",
        "tested_head": head,
        "reason": "The requested behavior already holds; no edit was required",
        "validation": [{"command": "test behavior", "exit_code": 0,
            "log_artifact": "validation.json"}],
    });
    let log = json!({"run_id": "batch-1", "tested_head": head,
        "command": "test behavior", "exit_code": 0, "output": "1 test passed"});
    let input = json!({"scope": "all", "job_run_id": "batch-1", "verify_already_landed": true,
        "workspace_path": temp.path(), "base_sha": head});
    Fixture {
        temp,
        task,
        input,
        report,
        log,
    }
}

fn artifacts(report: &Value, log: &Value) -> Vec<TaskArtifact> {
    vec![
        TaskArtifact::from_text("no-diff.json", report.to_string()),
        TaskArtifact::from_text("validation.json", log.to_string()),
    ]
}

fn object_inventory(workspace: &Path) -> String {
    git_output(workspace, &["rev-list", "--all", "--objects"]).unwrap()
}

fn assert_git_unmutated(workspace: &Path, head: &Value, objects: &str) {
    assert_eq!(
        json!(git_output(workspace, &["rev-parse", "HEAD"]).unwrap()),
        *head
    );
    assert_eq!(object_inventory(workspace), objects, "no new git object");
    assert!(
        git_output(workspace, &["status", "--porcelain"])
            .unwrap()
            .is_empty()
    );
}

#[test]
fn verified_no_diff_finishes_commit_without_a_commit_and_completes() {
    let Fixture {
        temp,
        task,
        input,
        report,
        log,
    } = fixture();
    let objects = object_inventory(temp.path());
    let host = CommitTestHost::new(vec![task], temp.path().to_path_buf())
        .with_artifacts(artifacts(&report, &log));

    let checkpoint = git_commit(&host, &input).expect("verified no-diff finishes commit");
    assert_eq!(checkpoint["decision"], "verified_no_diff");
    assert_eq!(checkpoint["committed"], false);
    assert_eq!(checkpoint["skipped_no_diff_expected"], true);
    assert!(checkpoint.get("commit_sha").is_none());
    assert_eq!(checkpoint["no_diff"], report);
    assert_eq!(checkpoint["validation_provenance"], json!([log]));
    assert_eq!(git_commit(&host, &input).unwrap(), checkpoint);
    assert_git_unmutated(temp.path(), &input["base_sha"], &objects);

    let mut handoff = input.clone();
    handoff["completed_task_ids"] = json!(["T1"]);
    handoff["no_diff_expected"] = json!(true);
    handoff["already_landed_checkpoint"] = checkpoint;
    pr_promote(&host, &handoff).unwrap();
    assert_eq!(host.get_task("T1").unwrap().status, TaskStatus::Review);
    let completed = pr_complete(&host, &handoff).unwrap();
    assert_eq!(completed["merge"]["reason"], "verified_no_diff");
    assert_eq!(completed["merge"]["merged"], false);
    assert_eq!(host.get_task("T1").unwrap().status, TaskStatus::Done);
    assert_git_unmutated(temp.path(), &input["base_sha"], &objects);
}

#[test]
fn clean_untagged_tree_without_no_diff_evidence_still_fails_nothing_to_commit() {
    for verify in [true, false] {
        let Fixture {
            temp,
            task,
            mut input,
            ..
        } = fixture();
        input["verify_already_landed"] = json!(verify);
        let objects = object_inventory(temp.path());
        let host = CommitTestHost::new(vec![task], temp.path().to_path_buf());
        let error = git_commit(&host, &input).expect_err("no evidence");
        assert!(
            error.to_string().contains("nothing to commit"),
            "verify={verify}: {error}"
        );
        assert_git_unmutated(temp.path(), &input["base_sha"], &objects);
    }
}

#[test]
fn no_diff_evidence_is_ignored_without_the_pipeline_recheck_flag() {
    let Fixture {
        temp,
        task,
        mut input,
        report,
        log,
    } = fixture();
    // Only a caller that rechecks evidence at promotion and completion may
    // accept it; the local route has no such recheck.
    input["verify_already_landed"] = json!(false);
    let host = CommitTestHost::new(vec![task], temp.path().to_path_buf())
        .with_artifacts(artifacts(&report, &log));
    let error = git_commit(&host, &input).unwrap_err();
    assert!(error.to_string().contains("nothing to commit"), "{error}");
}

#[test]
fn no_diff_refuses_invalid_evidence_without_mutating_git() {
    for fault in [
        "other_task",
        "other_head",
        "other_run",
        "empty_reason",
        "nonzero_exit",
        "no_validation",
        "duplicate_command",
        "missing_log",
        "stale_log",
        "failed_log",
        "log_for_other_command",
        "wrong_schema",
        "unknown_field",
    ] {
        let Fixture {
            temp,
            task,
            input,
            mut report,
            mut log,
        } = fixture();
        let objects = object_inventory(temp.path());
        match fault {
            "other_task" => report["task_id"] = json!("T2"),
            "other_head" => {
                let other = git_output(temp.path(), &["hash-object", "README.md"]).unwrap();
                report["tested_head"] = json!(other);
                log["tested_head"] = json!(other);
            }
            "other_run" => {
                report["run_id"] = json!("different-run");
                log["run_id"] = json!("different-run");
            }
            "empty_reason" => report["reason"] = json!(" "),
            "nonzero_exit" => report["validation"][0]["exit_code"] = json!(1),
            "no_validation" => report["validation"] = json!([]),
            "duplicate_command" => {
                let check = report["validation"][0].clone();
                report["validation"] = json!([check.clone(), check]);
            }
            "stale_log" => log["tested_head"] = json!("0".repeat(40)),
            "failed_log" => log["exit_code"] = json!(2),
            "log_for_other_command" => log["command"] = json!("other check"),
            "wrong_schema" => report["schema_version"] = json!(2),
            "unknown_field" => report["unexpected"] = json!(true),
            _ => {}
        }
        let mut stored = artifacts(&report, &log);
        if fault == "missing_log" {
            stored.pop();
        }
        let host =
            CommitTestHost::new(vec![task], temp.path().to_path_buf()).with_artifacts(stored);
        let error = git_commit(&host, &input).expect_err(fault).to_string();
        assert!(error.contains("nothing to commit"), "{fault}: {error}");
        assert!(error.contains("no_diff_unverified"), "{fault}: {error}");
        assert_eq!(host.get_task("T1").unwrap().status, TaskStatus::InProgress);
        assert_git_unmutated(temp.path(), &input["base_sha"], &objects);
    }
}

#[test]
fn no_diff_evidence_does_not_bypass_worktree_head_changed() {
    for tagged in [false, true] {
        let Fixture {
            temp,
            mut task,
            input,
            mut report,
            mut log,
        } = fixture();
        if tagged {
            task.tags.push(NO_DIFF_EXPECTED_TAG.to_string());
        }
        // HEAD leaves the pin for an unrelated root: not a descendant.
        git_success(temp.path(), &["checkout", "--orphan", "unrelated"]).unwrap();
        fs::write(temp.path().join("README.md"), "unrelated\n").unwrap();
        git_success(temp.path(), &["add", "README.md"]).unwrap();
        git_success(temp.path(), &["commit", "-m", "unrelated root"]).unwrap();
        let head = git_output(temp.path(), &["rev-parse", "HEAD"]).unwrap();
        report["tested_head"] = json!(head);
        log["tested_head"] = json!(head);
        let host = CommitTestHost::new(vec![task], temp.path().to_path_buf())
            .with_artifacts(artifacts(&report, &log));
        let error = git_commit(&host, &input).unwrap_err().to_string();
        assert!(
            error.contains("worktree_head_changed"),
            "tagged={tagged}: {error}"
        );
        assert_eq!(
            git_output(temp.path(), &["rev-parse", "HEAD"]).unwrap(),
            head
        );
    }
}

#[test]
fn no_diff_evidence_does_not_bypass_a_failed_execution_outcome() {
    let Fixture {
        temp,
        mut task,
        input,
        report,
        log,
    } = fixture();
    task.execution_summary = "Outcome: failed\n\nNo change could be validated.".to_string();
    let objects = object_inventory(temp.path());
    let host = CommitTestHost::new(vec![task], temp.path().to_path_buf())
        .with_artifacts(artifacts(&report, &log));
    let error = git_commit(&host, &input).unwrap_err().to_string();
    assert!(error.contains("failed"), "{error}");
    assert!(!error.contains("no_diff_unverified"), "{error}");
    assert_git_unmutated(temp.path(), &input["base_sha"], &objects);
}

#[test]
fn non_empty_staged_diff_is_committed_not_reported_as_no_diff() {
    let Fixture {
        temp,
        mut task,
        input,
        report,
        log,
    } = fixture();
    task.context_files = vec!["file:README.md".to_string()];
    fs::write(temp.path().join("README.md"), "changed\n").unwrap();
    let host = CommitTestHost::new(vec![task], temp.path().to_path_buf())
        .with_artifacts(artifacts(&report, &log));
    let result = git_commit(&host, &input).expect("a real diff commits");
    assert_eq!(result["decision"], "performed");
    assert_eq!(result["committed"], true);
    assert_eq!(result["skipped_no_diff_expected"], false);
    assert_ne!(
        json!(git_output(temp.path(), &["rev-parse", "HEAD"]).unwrap()),
        input["base_sha"]
    );
}

#[test]
fn no_diff_refuses_a_dirty_worktree() {
    let Fixture {
        temp,
        task,
        input,
        report,
        log,
    } = fixture();
    fs::write(temp.path().join("dirty.txt"), "pending work").unwrap();
    let host = CommitTestHost::new(vec![task], temp.path().to_path_buf())
        .with_artifacts(artifacts(&report, &log));
    let error = git_commit(&host, &input).unwrap_err().to_string();
    assert!(error.contains("unknown untracked paths"), "{error}");
    assert_eq!(
        json!(git_output(temp.path(), &["rev-parse", "HEAD"]).unwrap()),
        input["base_sha"]
    );
}

#[test]
fn no_diff_promotion_rechecks_tampering_and_dirty_tree() {
    let Fixture {
        temp,
        task,
        mut input,
        report,
        log,
    } = fixture();
    let host = CommitTestHost::new(vec![task], temp.path().to_path_buf())
        .with_artifacts(artifacts(&report, &log));
    input["already_landed_checkpoint"] = git_commit(&host, &input).unwrap();
    input["completed_task_ids"] = json!(["T1"]);
    input["no_diff_expected"] = json!(true);
    input["already_landed_checkpoint"]["no_diff"]["reason"] = json!("rewritten");
    let error = pr_promote(&host, &input).unwrap_err().to_string();
    assert!(error.contains("evidence changed"), "{error}");
    input["already_landed_checkpoint"]["no_diff"]["reason"] = report["reason"].clone();
    fs::write(temp.path().join("dirty.txt"), "pending work").unwrap();
    let error = pr_promote(&host, &input).unwrap_err().to_string();
    assert!(error.contains("not clean"), "{error}");
    assert_eq!(host.get_task("T1").unwrap().status, TaskStatus::InProgress);
}
