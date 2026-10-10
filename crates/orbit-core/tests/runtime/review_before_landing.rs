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

/// The ledger the fixture's admitted attempt reserved in, and whether its
/// candidate already had its one review.
fn candidate_reviewed(fixture: &Fixture) -> (orbit_types::workflow::ReviewLedger, bool) {
    let ledger = fixture
        .runtime
        .review_store()
        .unwrap()
        .review_ledger(
            &fixture.runtime.workspace_id().unwrap(),
            fixture.input["admission"]["lineage_key"].as_str().unwrap(),
        )
        .unwrap()
        .unwrap();
    let attempt = ledger.attempts.last().unwrap();
    let reviewed = ledger.reviewed(&attempt.candidate, &attempt.task_meaning_digest);
    (ledger, reviewed)
}

fn report(fixture: &Fixture, summary: &str, validation: Value, escalation: Value) -> Value {
    json!({
        "schema_version": REVIEW_CONTRACT_VERSION,
        "attempt_id": fixture.input["admission"]["attempt_id"],
        "verdict": "incomplete", "summary": summary,
        "findings": [], "validation": validation, "escalation": escalation,
    })
}

/// A reviewer that exits cleanly with only the placeholder it persisted first
/// produced no verdict [ORB-15130]. Its attempt is released with a typed
/// refusal, so the candidate's one review stays unspent and the same run can
/// admit a fresh reviewer; the failure handoff states the open PR and the task
/// status the store holds.
#[test]
fn a_reviewer_that_leaves_only_its_initial_report_does_not_spend_the_review() {
    if !super::dispatch_admission::isolated(
        "review_before_landing::a_reviewer_that_leaves_only_its_initial_report_does_not_spend_the_review",
    ) {
        return;
    }
    let mut fixture = Fixture::before_landing();
    published(&fixture);
    fixture.input["before_landing"] = json!(true);
    fixture.admit();
    let first_attempt = fixture.input["admission"]["attempt_id"].clone();
    fixture.put_report(&report(
        &fixture,
        "Review still running; validation not yet complete.",
        json!([]),
        Value::Null,
    ));

    let refused = fixture
        .settle()
        .expect_err("an abandoned review is not an approval")
        .to_string();
    assert!(
        refused.contains(orbit_types::workflow::REVIEW_ABANDONED_MARKER)
            && refused.contains("stays open and unmerged"),
        "{refused}"
    );
    let (ledger, reviewed) = candidate_reviewed(&fixture);
    assert!(
        ledger.attempts[0].released_at.is_some(),
        "the abandoned attempt is released, not settled"
    );
    assert!(
        !reviewed,
        "an abandoned attempt is not the candidate's review"
    );
    assert!(
        fixture
            .runtime
            .get_task_artifact(
                &fixture.task_id,
                orbit_types::workflow::REVIEW_GATE_ARTIFACT
            )
            .unwrap()
            .is_none(),
        "no certificate records a verdict nobody reached"
    );
    assert_eq!(status(&fixture), TaskStatus::Review);

    let handoff = failure_handoff(&fixture, "landing_review_gate_settle", &refused);
    assert_eq!(handoff["decision"], "landing_review_failure", "{handoff}");
    assert_eq!(handoff["reason"], "review_abandoned", "{handoff}");

    // The same run admits a reviewer again: the candidate's review is not
    // spent, so the released attempt resumes instead of being refused.
    fixture.admit();
    assert_eq!(fixture.input["admission"]["applies"], true);
    assert_eq!(fixture.input["admission"]["attempt_id"], first_attempt);
    fixture.put_report(&report(
        &fixture,
        "Checked the published PR.",
        json!([]),
        json!("The change is wrong."),
    ));
    fixture
        .settle()
        .expect_err("a genuine incomplete stops delivery");
    assert!(
        candidate_reviewed(&fixture).1,
        "the resumed attempt's own verdict is the candidate's review"
    );
    let comments = fixture.runtime.get_task_comments(&fixture.task_id).unwrap();
    let verdict = comments
        .iter()
        .rfind(|comment| {
            comment
                .message
                .starts_with("before-landing review settled attempt")
        })
        .unwrap_or_else(|| panic!("{comments:#?}"));
    assert!(
        verdict.message.contains("PR #42 stays open and unmerged")
            && verdict.message.contains("the task stays `review`")
            && !verdict.message.contains("no PR is opened")
            && !verdict.message.contains("the task is blocked"),
        "{}",
        verdict.message
    );
}

/// Anything the reviewer decided or revised is a real review, however
/// incomplete: a reason, a recorded check or a revision past the first
/// settles the attempt and spends the candidate's review as before.
#[test]
fn an_incomplete_review_with_a_reason_a_record_or_a_revision_still_counts() {
    if !super::dispatch_admission::isolated(
        "review_before_landing::an_incomplete_review_with_a_reason_a_record_or_a_revision_still_counts",
    ) {
        return;
    }
    let record = json!([{
        "id": "V1", "command": "fixture check", "outcome": "passed", "role": "required",
    }]);
    for case in ["escalation", "validation record", "second revision"] {
        let mut fixture = Fixture::before_landing();
        published(&fixture);
        fixture.input["before_landing"] = json!(true);
        fixture.admit();
        let placeholder = "Review still running; validation not yet complete.";
        match case {
            "escalation" => fixture.put_report(&report(
                &fixture,
                placeholder,
                json!([]),
                json!("Missing evidence."),
            )),
            "validation record" => {
                fixture.put_report(&report(&fixture, placeholder, record.clone(), Value::Null));
            }
            _ => {
                fixture.put_report(&report(&fixture, placeholder, json!([]), Value::Null));
                fixture.put_report(&report(&fixture, "Still reading.", json!([]), Value::Null));
            }
        }
        let refused = fixture
            .settle()
            .expect_err("an incomplete review stops delivery")
            .to_string();
        assert!(refused.contains("review_gate_blocked"), "{case}: {refused}");
        let (ledger, reviewed) = candidate_reviewed(&fixture);
        assert!(reviewed, "{case}: a settled incomplete is the review");
        assert!(ledger.attempts[0].released_at.is_none(), "{case}");
    }
}
