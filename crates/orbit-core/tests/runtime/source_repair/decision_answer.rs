//! Answering a block-triage option on a task whose failure handoff published
//! a candidate: a rewrite that changes the specification discards that
//! candidate with the answer instead of being refused (observed on ORB-22287),
//! an answer that leaves the specification alone keeps the candidate, and
//! the refusal that remains names a route the answering surfaces carry.
use super::handoff_requeue::{
    assert_next_run_resumes_the_published_candidate, delivery_forge, recorded, status,
};
use super::published_owner::{next, save_run};
use super::*;
use orbit_core::application::task::TaskUpdateParams;
use orbit_types::task::{
    CANDIDATE_DISCARDED_EVENT, DecisionAction, DecisionBinding, DecisionGate, DecisionRequest,
    DecisionRewrite, TaskRelationType, decision_option_answers,
};
use orbit_types::task::{DecisionOptionInput, DecisionRequestState};
use std::sync::Arc;

use super::failed_publication::{escalated_completion_fixture, escalated_handoff_fixture};

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

const FOLLOW_UP_TITLE: &str = "Migrate the stored totals";

/// Leave the fixture's publishing run in `state`. `decision_hold` records the
/// before-landing review step that ends a delivery `held` instead of `failed`.
fn park_source(f: &mut Fixture, state: JobRunState, decision_hold: bool) {
    let mut run = f.runtime.show_job_run_observed(&f.source.run_id).unwrap();
    run.state = state;
    if state.is_terminal() {
        run.pid = None;
        run.pid_start_time = None;
        run.finished_at = Some(Utc::now());
    } else {
        run.pid = Some(std::process::id());
        run.pid_start_time = None;
        run.finished_at = None;
    }
    save_run(f, &run);
    if decision_hold {
        let recorded = f
            .runtime
            .complete_job_run_step(
                &run.run_id,
                &orbit_store::contracts::JobRunStepParams {
                    step_index: 40,
                    target_type: orbit_types::workflow::JobTargetType::Activity,
                    target_id: "review_gate_settle".into(),
                    state: JobRunState::Held,
                    started_at: Utc::now(),
                    finished_at: Utc::now(),
                    duration_ms: Some(1),
                    exit_code: Some(0),
                    lock_wait_ms: None,
                    agent_response_json: None,
                    error_code: Some(
                        orbit_types::workflow::REVIEW_DECISION_PENDING_ERROR_CODE.into(),
                    ),
                    error_message: Some(format!(
                        "before-landing review did not approve; {}",
                        orbit_types::workflow::REVIEW_LANDING_DECISION_PENDING
                    )),
                },
            )
            .unwrap();
        assert!(recorded, "the review-decision step was recorded");
    }
    f.source = f.runtime.show_job_run_observed(&run.run_id).unwrap();
}

/// A block-triage request whose first option splits out a follow-up and, when
/// asked, rewrites this task's acceptance criteria.
fn raise_split(f: &Fixture, rewrite_criteria: bool) -> DecisionRequest {
    let blocked_at = f
        .runtime
        .get_task_history(&f.task)
        .unwrap()
        .iter()
        .rfind(|entry| entry.to_status == Some(TaskStatus::Blocked))
        .expect("the handoff blocked the task")
        .at;
    let rewrite = rewrite_criteria.then(|| DecisionRewrite {
        description: None,
        acceptance_criteria: Some(vec!["Recover only the stored totals".into()]),
    });
    let answers = decision_option_answers(
        &[
            option(
                "Split the migration out",
                rewrite,
                Some(DecisionAction::Split {
                    title: FOLLOW_UP_TITLE.into(),
                    description: "Move the stored totals to the new shape.".into(),
                    acceptance_criteria: vec!["Stored totals use the new shape.".into()],
                }),
            ),
            option("Archive the task", None, Some(DecisionAction::Archive)),
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

/// The incident: final recovery left the delivery `held` on the review
/// decision, and the split-and-rewrite answer was refused forever. The
/// answer now applies, files the follow-up and discards the stranded
/// candidate.
fn a_held_review_decision_split_applies_the_rewrite() {
    let (mut f, _) = escalated_handoff_fixture(None, None);
    park_source(&mut f, JobRunState::Held, true);
    let forge = delivery_forge(&f);
    f.runtime = f
        .runtime
        .clone()
        .with_task_pr_forge(Arc::new(forge.clone()));
    let request = raise_split(&f, true);

    answer(&f, &request, "1").unwrap();

    let task = f.runtime.get_task(&f.task).unwrap();
    assert_eq!(task.status, TaskStatus::Backlog);
    assert_eq!(task.acceptance_criteria, ["Recover only the stored totals"]);
    let follow_up = f
        .runtime
        .list_tasks()
        .unwrap()
        .into_iter()
        .find(|candidate| candidate.title == FOLLOW_UP_TITLE)
        .expect("the follow-up task was filed");
    assert!(
        follow_up.relations.iter().any(|relation| {
            relation.relation_type == TaskRelationType::SpawnedFrom
                && relation.target.as_str() == f.task
        }),
        "the follow-up is linked spawned_from: {:?}",
        follow_up.relations
    );
    assert_eq!(
        f.runtime
            .get_decision_request(&request.id)
            .unwrap()
            .unwrap()
            .state,
        DecisionRequestState::Answered
    );
    assert_eq!(recorded_by_anyone(&f, CANDIDATE_DISCARDED_EVENT), 1);
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

/// A requeue that leaves the specification alone resumes the pull request
/// the held review-decision run published.
fn a_held_review_decision_requeue_resumes_the_candidate() {
    let (mut f, _) = escalated_handoff_fixture(None, None);
    park_source(&mut f, JobRunState::Held, true);
    let request = raise_block_triage(&f);

    answer(&f, &request, "2").unwrap();

    assert_eq!(status(&f), TaskStatus::Backlog);
    assert_eq!(recorded_by_anyone(&f, CANDIDATE_DISCARDED_EVENT), 0);
    assert_eq!(recorded(&f, "published_owner_requeued"), 1);
    assert_next_run_resumes_the_published_candidate(&f);
}

/// A delivery that is still running is not repaired, and the refused split
/// files nothing. The message names cancel, which can stop that run.
fn a_running_delivery_refuses_the_split_without_filing_it() {
    let (mut f, _, _) = escalated_completion_fixture();
    park_source(&mut f, JobRunState::Running, false);
    let request = raise_split(&f, true);
    let tasks_before = f.runtime.list_tasks().unwrap().len();
    let history = f.runtime.get_task_history(&f.task).unwrap().len();
    let criteria = f.runtime.get_task(&f.task).unwrap().acceptance_criteria;

    let error = answer(&f, &request, "1").unwrap_err().to_string();

    assert!(
        error.contains("orbit run cancel")
            && error.contains(&f.source.run_id)
            && error.contains("--confirm"),
        "the refusal names cancel for the live run: {error}"
    );
    assert!(
        error.contains("is still running, so it cannot be repaired"),
        "the refusal says the delivery is still running: {error}"
    );
    assert!(
        !error.contains("requeue again when they pass"),
        "the refusal does not tell the operator to requeue a live run: {error}"
    );
    assert_eq!(f.runtime.list_tasks().unwrap().len(), tasks_before);
    assert!(
        f.runtime
            .list_tasks()
            .unwrap()
            .iter()
            .all(|task| task.title != FOLLOW_UP_TITLE),
        "a refused split filed no follow-up"
    );
    let task = f.runtime.get_task(&f.task).unwrap();
    assert_eq!(task.status, TaskStatus::Blocked);
    assert_eq!(task.acceptance_criteria, criteria);
    assert_eq!(f.runtime.get_task_history(&f.task).unwrap().len(), history);
    assert_eq!(
        f.runtime
            .get_decision_request(&request.id)
            .unwrap()
            .unwrap()
            .state,
        DecisionRequestState::Open
    );
}

/// A stopped hold that is not the review decision stays refused. Cancel
/// cannot change a terminal run; discard can, and the message names it.
fn a_held_run_without_a_review_decision_refuses_the_split() {
    let (mut f, _) = escalated_handoff_fixture(None, None);
    park_source(&mut f, JobRunState::Held, false);
    // Leave the specification unchanged so the refusal is the run state,
    // not the specification-mismatch that a rewrite reports first.
    let request = raise_split(&f, false);
    let tasks_before = f.runtime.list_tasks().unwrap().len();

    let error = answer(&f, &request, "1").unwrap_err().to_string();

    assert!(
        error.contains("not a conclusively stopped failed delivery") && error.contains("is held"),
        "the refusal names the held state: {error}"
    );
    assert!(
        error.contains("orbit task requeue") && error.contains("--discard-candidate"),
        "the refusal names discard, which succeeds on a blocked task: {error}"
    );
    assert!(
        !error.contains("orbit run cancel"),
        "cancel does not succeed on a terminal run: {error}"
    );
    assert_eq!(f.runtime.list_tasks().unwrap().len(), tasks_before);
    assert!(
        f.runtime
            .list_tasks()
            .unwrap()
            .iter()
            .all(|task| task.title != FOLLOW_UP_TITLE),
        "a refused split filed no follow-up"
    );
    assert_eq!(status(&f), TaskStatus::Blocked);
    assert_eq!(
        f.runtime.get_task(&f.task).unwrap().acceptance_criteria,
        ["Continue the same PR"]
    );
    assert_eq!(
        f.runtime
            .get_decision_request(&request.id)
            .unwrap()
            .unwrap()
            .state,
        DecisionRequestState::Open
    );
}

crate::scenario_table!(
    decision_answer_scenarios,
    a_rewrite_answer_discards_the_candidate_it_invalidates,
    an_answer_that_keeps_the_specification_keeps_the_candidate,
    a_remaining_refusal_names_a_route_the_answering_surface_has,
    a_held_review_decision_split_applies_the_rewrite,
    a_held_review_decision_requeue_resumes_the_candidate,
    a_running_delivery_refuses_the_split_without_filing_it,
    a_held_run_without_a_review_decision_refuses_the_split
);
