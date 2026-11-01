//! Pilot-warning triage through `orbit.task.block_triage`: `propose` — the
//! default whenever the operator resolves the hold by choosing — opens a
//! decision card on the held proposed task, `report` opens a single "Done"
//! card for an action outside Orbit, and a human's answer approves the task
//! into backlog and closes the request in one operation.

use orbit_types::task::{DecisionGate, DecisionRequestState, TaskStatus};
use serde_json::{Value, json};

use super::escalation_triage_crews::held_pilot_warning;
use super::recovery_fixture::{Fixture, fixture};

fn triage(fx: &Fixture, input: Value) -> Result<Value, String> {
    let mut input = input;
    input["model"] = json!("claude");
    input["workspace"] = json!(fx.repo.to_string_lossy());
    fx.runtime
        .run_tool("orbit.task.block_triage", input)
        .map_err(|error| error.to_string())
}

#[test]
fn a_proposal_opens_a_decision_card_and_a_human_answer_approves_the_task() {
    let fx = fixture();
    let task = held_pilot_warning(&fx, None);
    // An approval option is refused for a task the backlog would not admit, so
    // the held task carries the footprint admission needs.
    fx.update(json!({"id": task, "context_files": ["crates/orbit-web/assets/dashboard"], "allow_missing_context": true}));
    triage(
        &fx,
        json!({
            "action": "propose",
            "id": task,
            "summary": "The task reaches an unnamed surface, so the pilot will not promote it.",
            "options": [
                {
                    "summary": "Name the surface and approve",
                    "consequence": "The criteria name the surface and the task moves to backlog.",
                    "rewrite": {"acceptance_criteria": ["Triaged.", "Only the dashboard assets change."]},
                    "action": {"kind": "requeue"}
                },
                {
                    "summary": "Archive the task",
                    "consequence": "The task is dropped.",
                    "action": {"kind": "archive"}
                }
            ],
            "recommended": 1,
        }),
    )
    .unwrap();
    assert_eq!(fx.status(&task), "proposed", "proposing changes no status");
    let request = fx
        .runtime
        .list_decision_requests(None, Some(&task), false)
        .unwrap()
        .pop()
        .expect("the proposal opened a decision request");
    assert_eq!(request.gate, DecisionGate::ProposedApproval);
    assert!(
        request
            .summary
            .as_deref()
            .unwrap()
            .contains("unnamed surface"),
        "{request:?}"
    );
    assert_eq!(request.recommended.as_deref(), Some("1"));
    assert_eq!(request.answers.len(), 2);

    fx.human()
        .answer_decision_request_as_human(
            &request.id,
            "1",
            Some("Dashboard only.".to_string()),
            "human:operator".to_string(),
            Some("cli".to_string()),
        )
        .unwrap();
    let after = fx.runtime.get_task(&task).unwrap();
    assert_eq!(
        after.status,
        TaskStatus::Backlog,
        "approval moves it to backlog"
    );
    assert_eq!(
        after.acceptance_criteria,
        ["Triaged.", "Only the dashboard assets change."]
    );
    let comments = fx.runtime.get_task_comments(&task).unwrap();
    assert!(
        comments.iter().any(
            |comment| comment.message.contains("Name the surface and approve")
                && comment.message.contains("Dashboard only.")
        ),
        "the answer comment names the option and the note"
    );
    assert_eq!(
        fx.runtime
            .get_decision_request(&request.id)
            .unwrap()
            .unwrap()
            .state,
        DecisionRequestState::Answered
    );
}

#[test]
fn a_report_opens_one_done_card_with_the_exact_human_action() {
    let fx = fixture();
    let task = held_pilot_warning(&fx, None);
    triage(
        &fx,
        json!({
            "action": "report",
            "id": task,
            "summary": "The task needs a repository secret that only an owner can add.",
            "reason": "Add the DEPLOY_TOKEN secret in the repository settings.",
        }),
    )
    .unwrap();
    let request = fx
        .runtime
        .list_decision_requests(None, Some(&task), false)
        .unwrap()
        .pop()
        .expect("the report opened a decision request");
    assert_eq!(request.answers.len(), 1, "one Done option: {request:?}");
    assert!(
        request
            .human_action
            .as_deref()
            .unwrap()
            .contains("DEPLOY_TOKEN"),
        "{request:?}"
    );
    assert_eq!(fx.status(&task), "proposed");
}

#[test]
fn the_retired_block_subject_is_refused_by_the_triage_tool() {
    let fx = fixture();
    let task = fx.task(&[]);
    fx.escalate(&task);
    let error = triage(&fx, json!({"action": "candidates", "subject": "block"})).unwrap_err();
    assert!(
        error.contains("retired") && error.contains("final recovery"),
        "{error}"
    );
}
