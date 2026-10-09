//! A run that captured `review.before_landing` reviews its published PR, not
//! its unpublished candidate, and any outcome but an approve keeps that PR
//! open and unmerged with the task in review [ORB-14849].

use chrono::Utc;
use orbit_core::TaskStatus;
use orbit_engine::{RuntimeHost, TaskAutomationUpdate, execute_deterministic_action};
use orbit_types::task::{ExternalRef, GITHUB_PR_EXTERNAL_REF_SYSTEM};
use orbit_types::workflow::{JobRunState, REVIEW_CONTRACT_VERSION};
use serde_json::{Value, json};

use super::review_gate_audit::Fixture;

/// The pre-push gate does not apply to a before-landing run, the gate after
/// `pr_open` does, and a rejecting reviewer refuses its settlement. The
/// failure handoff then keeps the published PR and the task in review under
/// the failed step's typed reason — for a reject, an incomplete and a timed
/// out reviewer alike — and finalizing the failed run does not block it.
#[test]
fn a_before_landing_review_that_does_not_approve_keeps_the_pr_open_in_review() {
    if !super::dispatch_admission::isolated(
        "review_before_landing::a_before_landing_review_that_does_not_approve_keeps_the_pr_open_in_review",
    ) {
        return;
    }
    let mut fixture = Fixture::before_landing();
    let pre_push = fixture
        .runtime
        .run_deterministic(
            "review_gate_admit",
            &json!({}),
            &fixture.input,
            Default::default(),
        )
        .unwrap();
    assert_eq!(pre_push["applies"], false, "{pre_push}");
    assert_eq!(pre_push["reason"], "review_before_landing");
    assert_eq!(pre_push["timing"], "before-landing");

    // `pr_open` and `pr_promote` ran: the task is in review with its PR.
    published(&fixture);
    fixture.input["before_landing"] = json!(true);
    fixture.admit();
    let admission = &fixture.input["admission"];
    assert_eq!(admission["applies"], true, "{admission}");
    assert_eq!(admission["timing"], "before-landing");

    fixture.put_report(&json!({
        "schema_version": REVIEW_CONTRACT_VERSION,
        "attempt_id": fixture.input["admission"]["attempt_id"],
        "verdict": "reject", "summary": "Checked the published PR.",
        "findings": [],
        "validation": [{"id": "V1", "command": "fixture check", "outcome": "passed", "role": "required"}],
        "escalation": "The change is wrong.",
    }));
    let refused = fixture
        .settle()
        .expect_err("a rejecting review never approves the merge")
        .to_string();
    assert!(
        refused.contains("review_gate_blocked") && refused.contains("stays open and unmerged"),
        "{refused}"
    );
    assert_eq!(status(&fixture), TaskStatus::Review);

    for (failed_step, message, reason) in [
        (
            "landing_review_gate_settle",
            refused.as_str(),
            "review_gate_blocked",
        ),
        (
            "landing_review_gate_settle",
            "review_gate_blocked: verdict incomplete (missing evidence); 0 finding(s) recorded",
            "review_gate_blocked",
        ),
        (
            "landing_review",
            "review_timeout_incomplete: reviewer exceeded its wall clock",
            "review_timeout_incomplete",
        ),
    ] {
        let handoff = failure_handoff(&fixture, failed_step, message);
        assert_eq!(handoff["decision"], "landing_review_failure", "{handoff}");
        assert_eq!(handoff["reason"], reason, "{handoff}");
        assert_eq!(handoff["pr_number"], "42");
        assert_eq!(handoff["task_status"], "review");
        assert_eq!(status(&fixture), TaskStatus::Review);
    }
    let comments = fixture.runtime.get_task_comments(&fixture.task_id).unwrap();
    assert!(
        comments.iter().any(|comment| comment.message.starts_with(
            "Before-landing review did not approve PR #42 (`review_timeout_incomplete`)"
        )),
        "{comments:#?}"
    );

    let run_id = fixture.input["job_run_id"].as_str().unwrap();
    RuntimeHost::mark_job_run_running(&fixture.runtime, run_id, Utc::now(), std::process::id())
        .unwrap();
    RuntimeHost::finalize_job_run(
        &fixture.runtime,
        run_id,
        JobRunState::Failed,
        Utc::now(),
        None,
    )
    .unwrap();
    assert_eq!(
        status(&fixture),
        TaskStatus::Review,
        "the failed run leaves the reviewed PR's task in review"
    );
}

/// `pr_open` published PR #42 and `pr_promote` moved the task to review.
fn published(fixture: &Fixture) {
    fixture
        .runtime
        .apply_task_automation_update(
            &fixture.task_id,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Review),
                external_refs: vec![
                    ExternalRef::try_new(
                        GITHUB_PR_EXTERNAL_REF_SYSTEM.to_string(),
                        "42".to_string(),
                        None,
                    )
                    .unwrap(),
                ],
                ..TaskAutomationUpdate::default()
            },
        )
        .unwrap();
}

fn status(fixture: &Fixture) -> TaskStatus {
    fixture.runtime.get_task(&fixture.task_id).unwrap().status
}

fn failure_handoff(fixture: &Fixture, failed_step: &str, message: &str) -> Value {
    execute_deterministic_action(
        &fixture.runtime,
        "pr_failure_handoff",
        &json!({}),
        &json!({
            "failed_step_id": failed_step,
            "error_code": "deterministic_action_refused",
            "error_message": message,
            "run_id": fixture.input["job_run_id"],
            "job_input": {"task_ids": [fixture.task_id], "base_branch": "main", "base_sync": "local"},
            "pipeline": {
                "worktree": {"job_run_id": fixture.input["job_run_id"], "workspace_path": fixture.repo},
                "landing_review_gate_admit": fixture.input["admission"],
            },
        }),
        false,
        &Default::default(),
        None,
    )
    .unwrap()
}
