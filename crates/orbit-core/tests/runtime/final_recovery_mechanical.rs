//! Final recovery's bounded mechanical steps through `apply_final_recovery`:
//! `rerun_ci` re-requests the failed checks at the inspected head and requeues
//! the task, `resume_run` resumes the blocking run, and each runs at most once
//! per head or run. A refused step records its refusal and escalates, so the
//! human sees why the automatic step did not run.

use std::process::Command;

use chrono::Utc;
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_types::task::TaskStatus;
use orbit_types::workflow::JobRunState;
use serde_json::json;
use tempfile::TempDir;

use super::recovery_fixture::{RerunForge, fixture, fixture_with_forge};

const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";

fn rerun_ci_decision(head: &str) -> serde_json::Value {
    json!({
        "decision": "rerun_ci",
        "head_sha": head,
        "reason": "the failure is a runner network timeout",
    })
}

#[test]
fn rerun_ci_reruns_the_failed_checks_once_per_head_and_requeues() {
    let forge = RerunForge::new(HEAD, &["Test (shard 1)"]);
    let fx = fixture_with_forge(&forge);
    let task = fx.task(&[]);
    fx.record_pull_request(&task, 17);
    fx.update(json!({"id": task, "status": "in_progress"}));

    fx.final_recovery(&task, rerun_ci_decision(HEAD));
    assert_eq!(forge.reruns(), vec![(17, HEAD.to_string())]);
    assert_eq!(fx.status(&task), "backlog");
    let history = fx.runtime.get_task_history(&task).unwrap();
    let rerun = history
        .iter()
        .find(|entry| entry.event == "final_recovery_ci_rerun")
        .expect("the rerun is recorded in history");
    assert!(
        rerun
            .note
            .as_deref()
            .unwrap()
            .contains(&format!("head={HEAD}")),
        "{rerun:?}"
    );
    let comments = fx.runtime.get_task_comments(&task).unwrap();
    assert!(
        comments
            .iter()
            .any(|comment| comment.message.contains("Test (shard 1)")),
        "the comment names the failed check"
    );

    // The same head again: refused, nothing re-requested, the task escalates.
    fx.update(json!({"id": task, "status": "in_progress"}));
    fx.final_recovery(&task, rerun_ci_decision(HEAD));
    assert_eq!(forge.reruns().len(), 1, "a head is re-run once");
    assert_eq!(fx.status(&task), "blocked");
    let comments = fx.runtime.get_task_comments(&task).unwrap();
    assert!(
        comments
            .iter()
            .any(|comment| comment.message.contains("already re-run once")),
        "the escalation says why the step did not run"
    );
}

#[test]
fn rerun_ci_is_refused_when_the_head_moved() {
    let forge = RerunForge::new(HEAD, &["Test (shard 1)"]);
    let fx = fixture_with_forge(&forge);
    let task = fx.task(&[]);
    fx.record_pull_request(&task, 17);
    fx.update(json!({"id": task, "status": "in_progress"}));
    fx.final_recovery(
        &task,
        rerun_ci_decision("fedcba9876543210fedcba9876543210fedcba98"),
    );
    assert!(
        forge.reruns().is_empty(),
        "the inspected head is not the PR's head"
    );
    assert_eq!(fx.status(&task), "blocked");
}

#[test]
fn rerun_ci_is_refused_when_no_required_check_failed() {
    let forge = RerunForge::new(HEAD, &[]);
    let fx = fixture_with_forge(&forge);
    let task = fx.task(&[]);
    fx.record_pull_request(&task, 17);
    fx.update(json!({"id": task, "status": "in_progress"}));
    fx.final_recovery(&task, rerun_ci_decision(HEAD));
    assert!(
        forge.reruns().is_empty(),
        "no failed check, nothing to rerun"
    );
    assert_eq!(fx.status(&task), "blocked");
}

/// Run `test_name` alone in a child process with its own home, since the
/// substitute pipeline worker it installs is process-wide.
fn run_isolated_test(test_name: &str) -> bool {
    const CHILD: &str = "ORBIT_TEST_FINAL_RECOVERY_RESUME_CHILD";
    if std::env::var(CHILD).as_deref() == Ok(test_name) {
        return false;
    }
    let home = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    orbit_common::test_env::clear_inherited_authority(|name| {
        command.env_remove(name);
    });
    let output = command
        .args(["--exact", test_name, "--nocapture", "--test-threads=1"])
        .env(CHILD, test_name)
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .current_dir(home.path())
        .output()
        .unwrap();
    orbit_common::test_env::assert_child_test_passed(
        test_name,
        output.status,
        &output.stdout,
        &output.stderr,
    );
    true
}

#[test]
fn resume_run_resumes_the_failed_run_once_within_the_shared_bound() {
    if run_isolated_test(
        "final_recovery_mechanical::resume_run_resumes_the_failed_run_once_within_the_shared_bound",
    ) {
        return;
    }
    let fx = fixture();
    // Submitted runs execute in a substitute worker that outlives the test.
    let worker = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    crate::worker_fixture::install(worker.path(), "removed");
    let jobs_dir = fx.global.join("resources/jobs");
    std::fs::create_dir_all(&jobs_dir).unwrap();
    std::fs::write(
        jobs_dir.join("test_pipeline.yaml"),
        "schemaVersion: 2\nkind: Job\nmetadata:\n  name: test_pipeline\nspec:\n  state: enabled\n  steps: []\n",
    )
    .unwrap();

    let task = fx.task(&[]);
    let jobs = orbit_store::compose::workspace_job_run_store(
        fx.runtime.sqlite_store().unwrap(),
        fx.runtime.workspace_id().unwrap(),
    );
    let run = jobs
        .insert_job_run(
            "test_pipeline",
            1,
            Utc::now(),
            Some(json!({ "task_ids": [task] })),
            None,
        )
        .unwrap();
    // Its worker has exited, as a failed run's has.
    let mut exited = Command::new("true").spawn().unwrap();
    exited.wait().unwrap();
    jobs.mark_job_run_running(&run.run_id, Utc::now(), exited.id())
        .unwrap();
    jobs.finalize_job_run(&run.run_id, JobRunState::Failed, Utc::now(), Some(1))
        .unwrap();
    RuntimeHost::apply_task_automation_update(
        &fx.runtime,
        &task,
        TaskAutomationUpdate {
            status: Some(TaskStatus::InProgress),
            job_run_id: Some(run.run_id.clone()),
            ..Default::default()
        },
    )
    .unwrap();

    let resume = json!({
        "decision": "resume_run",
        "run_id": run.run_id,
        "reason": "the provider outage that failed the step has cleared",
    });
    fx.final_recovery_of(&task, &run.run_id, resume.clone());
    let resumed = jobs.job_run_retries(&run.run_id, 1).unwrap();
    assert_eq!(resumed.len(), 1, "the failed run was resumed once");
    assert_eq!(
        fx.runtime.get_task(&task).unwrap().status,
        TaskStatus::InProgress,
        "the resume readmitted the task"
    );
    let history = fx.runtime.get_task_history(&task).unwrap();
    let entry = history
        .iter()
        .find(|entry| entry.event == "final_recovery_resumed")
        .expect("the resume is recorded in history");
    let note = entry.note.as_deref().unwrap();
    assert!(
        note.contains(&format!("source_run_id={}", run.run_id))
            && note.contains(&format!("resumed_run_id={}", resumed[0].run_id)),
        "{note}"
    );

    // The same run again, from a later recovery: refused, nothing resumed,
    // the task escalates and says why.
    fx.final_recovery(&task, resume);
    assert_eq!(
        jobs.job_run_retries(&run.run_id, 1).unwrap().len(),
        1,
        "a run resumes once"
    );
    assert_eq!(fx.status(&task), "blocked");
    assert!(
        fx.runtime
            .get_task_comments(&task)
            .unwrap()
            .iter()
            .any(|comment| comment.message.contains("resume_run refused")),
        "the escalation says why the step did not run"
    );
}
