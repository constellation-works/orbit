//! Answering a block-triage option on a task whose failure handoff published
//! a candidate: a rewrite that changes the specification discards that
//! candidate with the answer instead of being refused (observed on ORB-22287),
//! an answer that leaves the specification alone keeps the candidate, and
//! the refusal that remains names a route the answering surfaces carry.
use super::handoff_requeue::{
    assert_next_run_resumes_the_published_candidate, delivery_forge, recorded, status,
};
use super::published_owner::next;
use super::*;
use orbit_core::application::task::TaskUpdateParams;
use orbit_types::task::{
    CANDIDATE_DISCARDED_EVENT, DecisionAction, DecisionBinding, DecisionGate, DecisionRequest,
    DecisionRewrite, decision_option_answers,
};
use orbit_types::task::{DecisionOptionInput, DecisionRequestState};
use std::sync::Arc;

use super::failed_publication::escalated_handoff_fixture;

fn option(
    summary: &str,
    rewrite: Option<DecisionRewrite>,
    action: Option<DecisionAction>,
) -> DecisionOptionInput {
    DecisionOptionInput {
        summary: summary.into(),
        consequence: format!("{summary} lands."),
        rewrite,
        action,
    }
}

/// Raise a block-triage request on the blocked fixture task with one option
/// that rewrites the description and one that only requeues.
pub(super) fn raise_block_triage(f: &Fixture) -> DecisionRequest {
    let blocked_at = f
        .runtime
        .get_task_history(&f.task)
        .unwrap()
        .iter()
        .rfind(|entry| entry.to_status == Some(TaskStatus::Blocked))
        .expect("the handoff blocked the task")
        .at;
    let answers = decision_option_answers(
        &[
            option(
                "Narrow the task",
                Some(DecisionRewrite {
                    description: Some("Repair only the remaining compiler error".into()),
                    acceptance_criteria: None,
                }),
                None,
            ),
            option(
                "Resume the candidate as it is",
                None,
                Some(DecisionAction::Requeue),
            ),
        ],
        1,
    )
    .unwrap();
    let request = DecisionRequest::new(
        f.task.clone(),
        DecisionGate::BlockTriage,
        answers,
        DecisionBinding::BlockEpisode {
            episode: blocked_at.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true),
        },
        "block_triage".to_string(),
        Utc::now().max(blocked_at),
    );
    f.runtime
        .apply_task_automation_update(
            &f.task,
            orbit_engine::TaskAutomationUpdate {
                append_history: vec![request.history_entry("system")],
                ..Default::default()
            },
        )
        .unwrap();
    request
}

fn answer(
    f: &Fixture,
    request: &DecisionRequest,
    option: &str,
) -> Result<(), orbit_core::OrbitError> {
    f.runtime
        .answer_decision_request_as_human(
            &request.id,
            option,
            None,
            "human:operator".to_string(),
            Some("cli".to_string()),
        )
        .map(|_| ())
}

/// The incident: the option's rewrite changed the specification the failure
/// handoff's pull request implements, and the answer was refused with a route
/// the answering surface could not use. The answer now applies the rewrite,
/// discards the candidate, closes its pull request and says so.
fn a_rewrite_answer_discards_the_candidate_it_invalidates() {
    let (mut f, _) = escalated_handoff_fixture(None, None);
    let forge = delivery_forge(&f);
    f.runtime = f
        .runtime
        .clone()
        .with_task_pr_forge(Arc::new(forge.clone()));
    let request = raise_block_triage(&f);

    answer(&f, &request, "1").unwrap();

    let task = f.runtime.get_task(&f.task).unwrap();
    assert_eq!(task.status, TaskStatus::Backlog);
    assert_eq!(task.description, "Repair only the remaining compiler error");
    assert_eq!(
        recorded_by_anyone(&f, CANDIDATE_DISCARDED_EVENT),
        1,
        "the discard is recorded in the task history"
    );
    let comments = f.runtime.get_task_comments(&f.task).unwrap();
    assert!(
        comments
            .iter()
            .any(|comment| comment.message.contains("Candidate discarded")),
        "the answer says it discarded the candidate: {comments:?}"
    );
    assert_eq!(
        forge
            .closed
            .lock()
            .unwrap()
            .iter()
            .map(|(number, _)| *number)
            .collect::<Vec<_>>(),
        [17],
        "the discarded candidate's pull request is closed"
    );
    assert_eq!(
        f.runtime
            .get_decision_request(&request.id)
            .unwrap()
            .unwrap()
            .state,
        DecisionRequestState::Answered
    );
    let (run, setup) = next(&f);
    let resumed = f.action(
        "candidate_resume",
        json!({"job_run_id":run.run_id,"task_ids":[f.task],
        "workspace_path":setup["workspace_path"],"base_sha":setup["base_sha"]}),
    );
    assert_eq!(
        resumed["implement"], true,
        "the rerun implements fresh: {resumed}"
    );
}

/// A candidate that still implements the task is not discarded by an answer
/// that leaves its specification alone.
fn an_answer_that_keeps_the_specification_keeps_the_candidate() {
    let (f, _) = escalated_handoff_fixture(None, None);
    let request = raise_block_triage(&f);

    answer(&f, &request, "2").unwrap();

    assert_eq!(status(&f), TaskStatus::Backlog);
    assert_eq!(recorded_by_anyone(&f, CANDIDATE_DISCARDED_EVENT), 0);
    assert_eq!(recorded(&f, "published_owner_requeued"), 1);
    assert_next_run_resumes_the_published_candidate(&f);
}

/// A specification already edited since the publication leaves the answer
/// refused with nothing written; the refusal names the discard routes the
/// answering surfaces carry (the dashboard's requeue and the CLI's).
fn a_remaining_refusal_names_a_route_the_answering_surface_has() {
    let (f, _) = escalated_handoff_fixture(None, None);
    let request = raise_block_triage(&f);
    f.runtime
        .update_task_as_human(
            &f.task,
            TaskUpdateParams {
                description: Some("Edited by hand after the publication".into()),
                ..Default::default()
            },
            "human".into(),
        )
        .unwrap();
    let history = f.runtime.get_task_history(&f.task).unwrap().len();

    let error = answer(&f, &request, "2").unwrap_err().to_string();

    assert!(
        error.contains("orbit task requeue") && error.contains("--discard-candidate"),
        "the refusal names the requeue discard route: {error}"
    );
    assert!(
        error.contains("dashboard"),
        "the refusal names the dashboard route: {error}"
    );
    assert_eq!(status(&f), TaskStatus::Blocked);
    assert_eq!(f.runtime.get_task_history(&f.task).unwrap().len(), history);
}

fn recorded_by_anyone(f: &Fixture, event: &str) -> usize {
    f.runtime
        .get_task_history(&f.task)
        .unwrap()
        .iter()
        .filter(|entry| entry.event == event)
        .count()
}

crate::scenario_table!(
    decision_answer_scenarios,
    a_rewrite_answer_discards_the_candidate_it_invalidates,
    an_answer_that_keeps_the_specification_keeps_the_candidate,
    a_remaining_refusal_names_a_route_the_answering_surface_has
);
