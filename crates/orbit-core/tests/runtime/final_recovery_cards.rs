//! Final recovery's decision card through its public boundaries: a `decide`
//! decision opens one decision request with a summary, two or three options
//! and a recommendation, a malformed option set is refused when the decision
//! is parsed, and a human's answer applies the chosen option's rewrite and
//! action, records the choice, and closes the request in one operation.

use orbit_types::task::{DecisionGate, DecisionRequestState, TaskRelationType, TaskStatus};
use orbit_types::workflow::FinalRecoveryDecision;
use serde_json::{Value, json};

use super::recovery_fixture::{Fixture, fixture};

/// The ORB-20598 shape: the real question is a data-semantics choice, so each
/// option rewrites an acceptance criterion and one also names an action.
fn data_semantics_decision() -> Value {
    json!({
        "decision": "decide",
        "summary": "Criterion 2 counts every task, but the dashboard total only counts active ones; the two cannot both hold.",
        "options": [
            {
                "summary": "Count only active tasks",
                "consequence": "The criterion matches the dashboard and the task returns to backlog.",
                "rewrite": {"acceptance_criteria": ["Recovered.", "The total counts active tasks only."]},
                "action": {"kind": "requeue"}
            },
            {
                "summary": "Count every task",
                "consequence": "The dashboard total changes, so the task is re-piloted first.",
                "rewrite": {"acceptance_criteria": ["Recovered.", "The total counts every task."]},
                "action": {"kind": "re_pilot"}
            },
            {
                "summary": "Reword the criterion only",
                "consequence": "The criterion is clarified and the task returns to backlog.",
                "rewrite": {"description": "Clarified: the total counts active tasks."}
            }
        ],
        "recommended": 1
    })
}

fn blocked_on_decision(fx: &Fixture, decision: Value) -> (String, String) {
    let task = fx.task(&[]);
    fx.update(json!({"id": task, "status": "in_progress"}));
    fx.final_recovery(&task, decision);
    assert_eq!(fx.status(&task), "blocked");
    let request = fx
        .runtime
        .list_decision_requests(None, Some(&task), false)
        .unwrap()
        .pop()
        .expect("final recovery opened a decision request");
    (task, request.id)
}

fn answer(fx: &Fixture, request: &str, option: &str, note: &str) {
    fx.human()
        .answer_decision_request_as_human(
            request,
            option,
            Some(note.to_string()),
            "human:operator".to_string(),
            Some("cli".to_string()),
        )
        .unwrap();
}

#[test]
fn a_decide_decision_opens_one_request_with_summary_options_and_recommendation() {
    let fx = fixture();
    let (task, request_id) = blocked_on_decision(&fx, data_semantics_decision());
    let request = fx
        .runtime
        .get_decision_request(&request_id)
        .unwrap()
        .unwrap();
    assert_eq!(request.gate, DecisionGate::BlockTriage);
    assert_eq!(request.state, DecisionRequestState::Open);
    assert!(
        request
            .summary
            .as_deref()
            .unwrap()
            .starts_with("Criterion 2 counts every task"),
        "{request:?}"
    );
    assert_eq!(request.recommended.as_deref(), Some("1"));
    assert_eq!(
        request
            .answers
            .iter()
            .map(|answer| answer.label.as_str())
            .collect::<Vec<_>>(),
        [
            "Count only active tasks",
            "Count every task",
            "Reword the criterion only"
        ]
    );
    // The task read carries the whole card, so a surface needs no second read.
    let shown = fx.show(&task);
    let open = &shown["open_decisions"][0];
    assert_eq!(open["id"], request_id.as_str());
    assert_eq!(open["options"].as_array().unwrap().len(), 3, "{open}");
    assert_eq!(open["options"][0]["recommended"], true);
    assert_eq!(open["options"][1]["recommended"], false);
}

#[test]
fn a_malformed_option_set_is_refused_when_the_decision_is_parsed() {
    let ok = data_semantics_decision();
    assert!(FinalRecoveryDecision::parse(&ok).is_ok());

    let mut neither = ok.clone();
    neither["options"][2] = json!({"summary": "Do nothing", "consequence": "Nothing changes."});
    let error = FinalRecoveryDecision::parse(&neither)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("rewrite") && error.contains("action"),
        "{error}"
    );

    let mut unknown = ok.clone();
    unknown["options"][0]["action"] = json!({"kind": "delete_everything"});
    assert!(FinalRecoveryDecision::parse(&unknown).is_err());

    let mut one = ok.clone();
    one["options"].as_array_mut().unwrap().truncate(1);
    assert!(
        FinalRecoveryDecision::parse(&one).is_err(),
        "one option is no choice"
    );

    let mut out_of_range = ok;
    out_of_range["recommended"] = json!(4);
    assert!(FinalRecoveryDecision::parse(&out_of_range).is_err());
}

#[test]
fn a_requeue_option_rewrites_comments_the_choice_and_closes_the_request() {
    let fx = fixture();
    let (task, request_id) = blocked_on_decision(&fx, data_semantics_decision());
    answer(&fx, &request_id, "1", "Dashboard is the source of truth.");

    let after = fx.runtime.get_task(&task).unwrap();
    assert_eq!(after.status, TaskStatus::Backlog);
    assert_eq!(
        after.acceptance_criteria,
        ["Recovered.", "The total counts active tasks only."]
    );
    let comments = fx.runtime.get_task_comments(&task).unwrap();
    let comment = comments
        .iter()
        .find(|comment| comment.message.starts_with("Decision answer:"))
        .expect("the answer is recorded as a comment");
    assert_eq!(comment.by, "human:operator");
    assert!(
        comment.message.contains("Count only active tasks")
            && comment
                .message
                .contains("Dashboard is the source of truth."),
        "the comment names the option and the note: {}",
        comment.message
    );
    assert_eq!(
        fx.runtime
            .get_decision_request(&request_id)
            .unwrap()
            .unwrap()
            .state,
        DecisionRequestState::Answered
    );
    assert!(
        fx.runtime
            .list_decision_requests(None, Some(&task), false)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_rewrite_only_option_edits_the_task_and_unblocks_it() {
    let fx = fixture();
    let (task, request_id) = blocked_on_decision(&fx, data_semantics_decision());
    answer(&fx, &request_id, "3", "Wording fix is enough.");
    let after = fx.runtime.get_task(&task).unwrap();
    assert_eq!(after.status, TaskStatus::Backlog);
    assert_eq!(
        after.description,
        "Clarified: the total counts active tasks."
    );
    assert_eq!(
        after.acceptance_criteria,
        ["Recovered."],
        "a field the option does not rewrite is left alone"
    );
}

#[test]
fn a_split_option_files_a_linked_follow_up_and_narrows_the_task() {
    let fx = fixture();
    let (task, request_id) = blocked_on_decision(
        &fx,
        json!({
            "decision": "decide",
            "summary": "The task bundles the API change with a migration that cannot ship together.",
            "options": [
                {
                    "summary": "Split the migration out",
                    "consequence": "A follow-up task owns the migration; this task keeps the API change.",
                    "rewrite": {"acceptance_criteria": ["Recovered."]},
                    "action": {
                        "kind": "split",
                        "title": "Migrate the stored totals",
                        "description": "Move the stored totals to the new shape.",
                        "acceptance_criteria": ["Stored totals use the new shape."]
                    }
                },
                {
                    "summary": "Archive the task",
                    "consequence": "The work is dropped.",
                    "action": {"kind": "archive"}
                }
            ],
            "recommended": 1
        }),
    );
    answer(&fx, &request_id, "1", "Ship them separately.");

    let after = fx.runtime.get_task(&task).unwrap();
    assert_eq!(after.status, TaskStatus::Backlog);
    let tasks = fx.runtime.list_tasks().unwrap();
    let follow_up = tasks
        .iter()
        .find(|candidate| candidate.title == "Migrate the stored totals")
        .expect("the follow-up task was filed");
    assert_eq!(
        follow_up.acceptance_criteria,
        ["Stored totals use the new shape."]
    );
    assert!(
        follow_up.relations.iter().any(|relation| {
            relation.relation_type == TaskRelationType::SpawnedFrom
                && relation.target.as_str() == task
        }),
        "the follow-up is linked spawned_from: {:?}",
        follow_up.relations
    );
}

#[test]
fn an_agent_attributed_answer_is_refused_and_changes_nothing() {
    let fx = fixture();
    let (task, request_id) = blocked_on_decision(&fx, data_semantics_decision());
    for label in ["claude", "system", "task-pilot"] {
        let error = fx
            .human()
            .answer_decision_request_as_human(&request_id, "1", None, label.to_string(), None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("automation"), "{label}: {error}");
    }
    assert_eq!(fx.status(&task), "blocked");
    assert_eq!(
        fx.runtime
            .get_decision_request(&request_id)
            .unwrap()
            .unwrap()
            .state,
        DecisionRequestState::Open
    );
}
