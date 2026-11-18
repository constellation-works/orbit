//! Final recovery's decision card through its public boundaries: a `decide`
//! decision opens one decision request with a summary, two or three options
//! and a recommendation, a malformed option set is refused when the decision
//! is parsed, and a human's answer applies the chosen option's rewrite and
//! action, records the choice, and closes the request in one operation.

use orbit_common::{DecisionRefusalCode, OrbitError};
use orbit_types::task::{DecisionGate, DecisionRequestState, TaskRelationType, TaskStatus};
use orbit_types::workflow::FinalRecoveryDecision;
use serde_json::{Value, json};

use orbit_engine::RuntimeHost as _;

use super::recovery_fixture::{Fixture, fixture};

/// A forge whose pull-request listing, the first step of closing a rejected
/// task's pull requests, records what the task and its decision request read
/// at that moment, then fails as an interrupted closure would.
struct ObservingForge {
    runtime: std::sync::Mutex<Option<orbit_core::OrbitRuntime>>,
    task: String,
    request: String,
    seen: std::sync::Mutex<Vec<(TaskStatus, DecisionRequestState)>>,
}

impl orbit_core::runtime::task_pr_forge::TaskPrForge for ObservingForge {
    fn authenticated_login(&self, _: &std::path::Path) -> Result<String, OrbitError> {
        Ok("orbit-bot".into())
    }

    fn close_pull_request(&self, _: &std::path::Path, _: u64, _: &str) -> Result<(), OrbitError> {
        Ok(())
    }

    fn open_pull_requests(
        &self,
        _: &std::path::Path,
    ) -> Result<Vec<orbit_core::runtime::task_pr_forge::ForgePullRequest>, OrbitError> {
        let runtime = self.runtime.lock().unwrap().clone().expect("runtime set");
        let status = runtime.get_task(&self.task).unwrap().status;
        let state = runtime
            .get_decision_request(&self.request)
            .unwrap()
            .unwrap()
            .state;
        self.seen.lock().unwrap().push((status, state));
        Err(OrbitError::Execution("forge interrupted".into()))
    }
}

struct ReconsideringForge {
    runtime: orbit_core::OrbitRuntime,
    task: String,
    request: String,
    reconsider: bool,
    closed: std::sync::Mutex<Vec<u64>>,
}

impl orbit_core::runtime::task_pr_forge::TaskPrForge for ReconsideringForge {
    fn authenticated_login(&self, _: &std::path::Path) -> Result<String, OrbitError> {
        Ok("orbit-bot".into())
    }

    fn close_pull_request(
        &self,
        _: &std::path::Path,
        number: u64,
        _: &str,
    ) -> Result<(), OrbitError> {
        self.closed.lock().unwrap().push(number);
        Ok(())
    }

    fn open_pull_requests(
        &self,
        _: &std::path::Path,
    ) -> Result<Vec<orbit_core::runtime::task_pr_forge::ForgePullRequest>, OrbitError> {
        assert_eq!(
            self.runtime.get_task(&self.task).unwrap().status,
            TaskStatus::Rejected
        );
        assert_eq!(
            self.runtime
                .get_decision_request(&self.request)
                .unwrap()
                .unwrap()
                .state,
            DecisionRequestState::Answered
        );
        if self.reconsider {
            self.runtime
                .run_tool(
                    "orbit.task.update",
                    json!({"id": self.task, "status": "backlog", "model": "codex"}),
                )
                .unwrap();
            self.runtime
                .apply_task_automation_update(
                    &self.task,
                    orbit_engine::TaskAutomationUpdate {
                        external_refs: vec![
                            orbit_types::task::ExternalRef::github_pr("43").unwrap(),
                        ],
                        ..Default::default()
                    },
                )
                .unwrap();
        }
        let number = if self.reconsider { 43 } else { 42 };
        Ok(vec![orbit_core::runtime::task_pr_forge::ForgePullRequest {
            number,
            head_branch: format!("orbit/{}-aaaa1111", self.task),
            body: format!("Task: {}", self.task),
            author: "orbit-bot".into(),
        }])
    }
}

fn deferred_rejection_respects_reconsideration() {
    for reconsider in [false, true] {
        let fx = fixture();
        let mut decision = data_semantics_decision();
        decision["options"][0]
            .as_object_mut()
            .unwrap()
            .remove("rewrite");
        decision["options"][0]["action"] = json!({"kind": "reject"});
        let (task, request) = blocked_on_decision(&fx, decision);
        fx.record_pull_request(&task, 42);
        let forge = std::sync::Arc::new(ReconsideringForge {
            runtime: fx.runtime.clone(),
            task: task.clone(),
            request: request.clone(),
            reconsider,
            closed: std::sync::Mutex::new(Vec::new()),
        });
        fx.human()
            .with_task_pr_forge(forge.clone())
            .answer_decision_request_as_human(&request, "1", None, "human:operator".into(), None)
            .unwrap();
        assert_eq!(
            *forge.closed.lock().unwrap(),
            if reconsider { vec![] } else { vec![42] }
        );
        let history = fx.runtime.get_task_history(&task).unwrap();
        assert_eq!(
            history
                .iter()
                .filter(|entry| entry.event == orbit_types::task::DECISION_ANSWERED_EVENT)
                .count(),
            1
        );
    }
}

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

fn a_decide_decision_opens_one_request_with_summary_options_and_recommendation() {
    let fx = fixture();
    let (task, request_id) = blocked_on_decision(&fx, data_semantics_decision());
    let request = fx
        .runtime
        .get_decision_request(&request_id)
        .unwrap()
        .unwrap();
    assert_eq!(request.gate, DecisionGate::BlockTriage);
    assert_eq!(
        request.risk_tier,
        orbit_types::task::DecisionRiskTier::Reversible
    );
    assert_eq!(
        request.category.as_deref(),
        Some(format!("{}/block_triage", fx.runtime.workspace_id().unwrap()).as_str())
    );
    assert!(
        request
            .answers
            .iter()
            .all(|answer| answer.risk_tier == orbit_types::task::DecisionRiskTier::Reversible)
    );
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

fn a_terminal_recovery_option_makes_the_request_irreversible() {
    let fx = fixture();
    let mut decision = data_semantics_decision();
    decision["options"][0]
        .as_object_mut()
        .unwrap()
        .remove("rewrite");
    decision["options"][0]["action"] = json!({"kind": "reject"});
    let (_, request_id) = blocked_on_decision(&fx, decision);
    let request = fx
        .runtime
        .get_decision_request(&request_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        request.answers[0].risk_tier,
        orbit_types::task::DecisionRiskTier::Irreversible
    );
    assert_eq!(
        request.answers[1].risk_tier,
        orbit_types::task::DecisionRiskTier::Reversible
    );
    assert_eq!(
        request.risk_tier,
        orbit_types::task::DecisionRiskTier::Irreversible
    );
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
                    "rewrite": {"acceptance_criteria": ["The API change stays in this task."]},
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
    assert_eq!(
        after.acceptance_criteria,
        ["The API change stays in this task."],
        "the split option's rewrite narrows the original task"
    );
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

/// A human escalation's block request offers only the requeue it asks for. It
/// has no failure-handoff receipt, so handoff landing derives no `land` answer
/// and leaves the request's risk tier reversible, with no pre-landing review.
fn an_escalation_without_a_handoff_receipt_offers_no_land_answer() {
    let fx = fixture();
    let task = fx.task(&[]);
    fx.escalate(&task);
    let listed = fx
        .runtime
        .list_decision_requests(None, Some(&task), false)
        .unwrap()
        .pop()
        .expect("escalation opened a decision request");
    let fetched = fx
        .runtime
        .get_decision_request(&listed.id)
        .unwrap()
        .unwrap();
    for request in [listed, fetched] {
        assert!(
            request.answers.iter().all(|answer| answer.id != "land"),
            "no receipt binds a candidate to land: {:?}",
            request.answers
        );
        assert_eq!(
            request.risk_tier,
            orbit_types::task::DecisionRiskTier::Reversible,
            "{request:?}"
        );
        assert_eq!(request.state, DecisionRequestState::Open);
    }
}

/// A reject option on a block-triage request is bound to the blocked task, so
/// answering it must reject that blocked task once: a terminal status, an
/// auditable history entry, the request answered, and no second answer.
fn a_reject_option_settles_the_blocked_task_once() {
    let fx = fixture();
    let mut decision = data_semantics_decision();
    decision["options"][0]
        .as_object_mut()
        .unwrap()
        .remove("rewrite");
    decision["options"][0]["action"] = json!({"kind": "reject"});
    let (task, request_id) = blocked_on_decision(&fx, decision);

    answer(&fx, &request_id, "1", "Superseded by another task.");

    let after = fx.runtime.get_task(&task).unwrap();
    assert_eq!(after.status, TaskStatus::Rejected);
    let history = fx.runtime.get_task_history(&task).unwrap();
    let rejections = history
        .iter()
        .filter(|entry| entry.event == "blocked_rejected")
        .count();
    assert_eq!(rejections, 1, "{history:?}");
    assert_eq!(
        history
            .iter()
            .filter(|entry| entry.event == orbit_types::task::DECISION_ANSWERED_EVENT)
            .count(),
        1,
        "{history:?}"
    );
    assert_eq!(
        fx.runtime
            .get_decision_request(&request_id)
            .unwrap()
            .unwrap()
            .state,
        DecisionRequestState::Answered
    );
    let again = fx
        .human()
        .answer_decision_request_as_human(
            &request_id,
            "1",
            None,
            "human:operator".to_string(),
            None,
        )
        .unwrap_err();
    assert!(
        matches!(
            again,
            OrbitError::DecisionRefused {
                code: DecisionRefusalCode::RequestSuperseded,
                ..
            }
        ),
        "a second answer is refused as superseded: {again}"
    );
    assert_eq!(
        fx.runtime.get_task(&task).unwrap().status,
        TaskStatus::Rejected
    );
}

/// The blocked reject commits its status and the answered request in one
/// store update, before the forge lookup that closes the task's pull
/// requests. An interruption in that lookup must leave the rejected task with
/// its request already answered, never a rejection that supersedes the request.
fn a_reject_option_records_its_resolution_before_pull_request_closure() {
    let fx = fixture();
    let mut decision = data_semantics_decision();
    decision["options"][0]
        .as_object_mut()
        .unwrap()
        .remove("rewrite");
    decision["options"][0]["action"] = json!({"kind": "reject"});
    let (task, request_id) = blocked_on_decision(&fx, decision);
    fx.runtime
        .apply_task_automation_update(
            &task,
            orbit_engine::TaskAutomationUpdate {
                external_refs: vec![orbit_types::task::ExternalRef::github_pr("42").unwrap()],
                ..Default::default()
            },
        )
        .expect("record the delivery PR");
    let forge = std::sync::Arc::new(ObservingForge {
        runtime: std::sync::Mutex::new(None),
        task: task.clone(),
        request: request_id.clone(),
        seen: std::sync::Mutex::new(Vec::new()),
    });
    let human = fx.human().with_task_pr_forge(forge.clone());
    *forge.runtime.lock().unwrap() = Some(human.clone());

    human
        .answer_decision_request_as_human(
            &request_id,
            "1",
            Some("Superseded by another task.".to_string()),
            "human:operator".to_string(),
            None,
        )
        .expect("a failed pull-request closure does not fail the answer");

    assert_eq!(
        *forge.seen.lock().unwrap(),
        vec![(TaskStatus::Rejected, DecisionRequestState::Answered)],
        "the closure ran once, after the rejection and its resolution were both durable"
    );
    let history = human.get_task_history(&task).unwrap();
    for event in [
        "blocked_rejected",
        orbit_types::task::DECISION_ANSWERED_EVENT,
    ] {
        assert_eq!(
            history.iter().filter(|entry| entry.event == event).count(),
            1,
            "{event}: {history:?}"
        );
    }
}

/// A block-triage reject is refused while a live local run still owns the
/// blocked task, as the archive and requeue answers on the same request are:
/// the request stays open and the task stays blocked.
fn a_reject_option_is_refused_while_a_live_run_owns_the_blocked_task() {
    let fx = fixture();
    let mut decision = data_semantics_decision();
    decision["options"][0]
        .as_object_mut()
        .unwrap()
        .remove("rewrite");
    decision["options"][0]["action"] = json!({"kind": "reject"});
    let (task, request_id) = blocked_on_decision(&fx, decision);
    let jobs = fx.runtime.stores().jobs();
    let now = chrono::Utc::now();
    let run = jobs
        .insert_job_run(
            "task_pr_pipeline",
            1,
            now,
            Some(json!({"task_ids": [task]})),
            None,
        )
        .unwrap();
    jobs.mark_job_run_running(&run.run_id, now, std::process::id())
        .unwrap();

    let refused = fx
        .human()
        .answer_decision_request_as_human(
            &request_id,
            "1",
            Some("Superseded by another task.".to_string()),
            "human:operator".to_string(),
            Some("cli".to_string()),
        )
        .unwrap_err();
    let refusal = refused
        .force_overridable()
        .expect("a live run refuses the reject with a typed refusal");
    assert_eq!(
        refusal.kind,
        orbit_common::ForceOverridableKind::RunStillOwnsTask
    );
    assert_eq!(fx.status(&task), "blocked");
    assert_eq!(
        fx.runtime
            .get_decision_request(&request_id)
            .unwrap()
            .unwrap()
            .state,
        DecisionRequestState::Open
    );
    let history = fx.runtime.get_task_history(&task).unwrap();
    assert!(
        history
            .iter()
            .all(|entry| entry.event != "blocked_rejected"),
        "{history:?}"
    );
}

crate::scenario_table!(
    final_recovery_cards,
    a_decide_decision_opens_one_request_with_summary_options_and_recommendation,
    a_terminal_recovery_option_makes_the_request_irreversible,
    a_reject_option_settles_the_blocked_task_once,
    a_reject_option_records_its_resolution_before_pull_request_closure,
    deferred_rejection_respects_reconsideration,
    a_reject_option_is_refused_while_a_live_run_owns_the_blocked_task,
    a_requeue_option_rewrites_comments_the_choice_and_closes_the_request,
    a_rewrite_only_option_edits_the_task_and_unblocks_it,
    a_split_option_files_a_linked_follow_up_and_narrows_the_task,
    an_agent_attributed_answer_is_refused_and_changes_nothing,
    an_escalation_without_a_handoff_receipt_offers_no_land_answer,
);
