//! A delivery held on a read-only forge observation after final recovery
//! parked it (observed on ORB-28400): the block-triage decision offers no
//! requeue the held source would refuse, names the resume that keeps the
//! candidate, and an older requeue answer is refused with that resume.
use super::decision_answer::{answer, park_source, raise_block_triage, recorded_by_anyone};
use super::failed_publication::escalated_completion_fixture;
use super::handoff_requeue::status;
use super::*;
use orbit_core::application::job::JobRunListParams;
use orbit_core::application::task::TaskUpdateParams;
use orbit_types::task::{
    CANDIDATE_DISCARDED_EVENT, DecisionAction, DecisionGate, DecisionRequest, DecisionRequestState,
};

/// The run the incident left: a published owner delivery whose completion
/// could not read the forge. Final recovery parks it through the host with
/// the engine's completion-observation report, and the run ends `held` as
/// `delivery_forge_observation_unavailable` (observed on ORB-28400).
fn held_observation_fixture() -> Fixture {
    use orbit_engine::{
        FinalRecoveryAdmission, FinalRecoveryAdmissionRequest, FinalRecoveryApplication,
        FinalRecoveryApplied, Recovery,
    };
    let mut f = Fixture::new();
    f.enable_final_recovery();
    let checkout = super::published_owner::publication(&mut f);
    let host = orbit_core::test_support::EngineHost::new(&f.runtime);
    let admission = Recovery::admit_final_recovery(
        &host,
        &f.source.run_id,
        &FinalRecoveryAdmissionRequest {
            task_id: f.task.clone(),
            failed_step_id: "complete_pr".into(),
            base_ref: Some("main".into()),
        },
    )
    .unwrap();
    assert_eq!(admission, FinalRecoveryAdmission::Admitted);
    let applied = Recovery::apply_final_recovery(
        &host,
        &f.source.run_id,
        &FinalRecoveryApplication {
            fault: Default::default(),
            task_id: f.task.clone(),
            failed_step_id: "complete_pr".into(),
            decision: orbit_types::workflow::FinalRecoveryDecision::Escalate {
                diagnosis: "unchanged completion reuse refused: candidate-changed: host has no \
                            authenticated completion prefix; original error: TLS handshake \
                            timeout"
                    .into(),
                human_action: "Inspect the publication and admit current-policy source repair \
                               when its evidence or inputs changed."
                    .into(),
                summary: None,
            },
            resume_step_index: None,
            preserved_steps: Vec::new(),
            repair_commit: None,
            resume_context: None,
            workspace_path: checkout,
            completion_done: false,
            infrastructure: None,
            completion_observation: true,
        },
    )
    .unwrap();
    assert!(
        matches!(applied, FinalRecoveryApplied::Escalated { .. }),
        "{applied:?}"
    );
    hold_on_forge_observation(&mut f);
    f
}

/// End the fixture's run `held` on the forge observation, as the engine does
/// after the park.
fn hold_on_forge_observation(f: &mut Fixture) {
    park_source(f, JobRunState::Held, false);
    let code = orbit_types::workflow::DeliveryRefusal::ForgeObservationUnavailable.error_code();
    assert!(
        orbit_core::test_support::EngineHost::new(&f.runtime)
            .complete_job_run_step(
                &f.source.run_id,
                &orbit_store::contract::JobRunStepParams {
                    step_index: 41,
                    target_type: orbit_types::workflow::JobTargetType::Activity,
                    target_id: "complete_pr".into(),
                    state: JobRunState::Held,
                    started_at: Utc::now(),
                    finished_at: Utc::now(),
                    duration_ms: Some(1),
                    exit_code: Some(1),
                    lock_wait_ms: None,
                    agent_response_json: None,
                    error_code: Some(code.into()),
                    error_message: Some("completion could not read the forge".into()),
                },
            )
            .unwrap()
    );
    f.source = f.runtime.show_job_run_observed(&f.source.run_id).unwrap();
}

fn open_request(f: &Fixture) -> DecisionRequest {
    let open: Vec<_> = f
        .runtime
        .get_task_decision_requests(&f.task)
        .unwrap()
        .into_iter()
        .filter(|request| request.state == DecisionRequestState::Open)
        .collect();
    assert_eq!(open.len(), 1, "one open request: {open:?}");
    open.into_iter().next().unwrap()
}

/// What the run left for the next attempt: its publication, task state and
/// the hold itself.
fn held_snapshot(f: &Fixture) -> (Value, Value, JobRunState, usize) {
    (
        serde_json::to_value(f.runtime.get_task(&f.task).unwrap()).unwrap(),
        serde_json::to_value(f.runtime.get_task_history(&f.task).unwrap()).unwrap(),
        f.runtime
            .show_job_run_observed(&f.source.run_id)
            .unwrap()
            .state,
        f.runtime
            .list_job_runs(JobRunListParams::default())
            .unwrap()
            .len(),
    )
}

/// The decision as presented: no requeue the held source would refuse, an
/// answer that keeps the candidate, and the exact resume that re-reads the
/// pull request. The task readout names the same resume.
fn a_held_forge_observation_offers_the_resume_instead_of_a_requeue() {
    let f = held_observation_fixture();
    let request = open_request(&f);
    let run = &f.source.run_id;

    assert_eq!(request.gate, DecisionGate::BlockTriage);
    assert!(
        request.answers.iter().all(|answer| answer
            .option
            .as_ref()
            .is_none_or(|option| option.action != Some(DecisionAction::Requeue))),
        "no requeue is offered for a held source: {:?}",
        request.answers
    );
    assert_eq!(
        request
            .answers
            .iter()
            .map(|answer| answer.id.as_str())
            .collect::<Vec<_>>(),
        ["dismiss"]
    );
    assert_eq!(request.recommended.as_deref(), Some("dismiss"));
    let action = request.human_action.as_deref().unwrap();
    assert!(
        action.contains(&format!("orbit job resume {run}"))
            && action.contains("delivery_forge_observation_unavailable"),
        "the human action names the resume: {action}"
    );
    let task = f.runtime.get_task(&f.task).unwrap();
    let history = f.runtime.get_task_history(&f.task).unwrap();
    let next = f.runtime.blocked_next_step(&task, &history).unwrap();
    assert_eq!(
        next.resume_command.as_deref(),
        Some(format!("orbit job resume {run}").as_str())
    );

    // The answer as attempted: an automation caller is refused, and the
    // human's answer keeps the task blocked with its candidate and hold.
    let before = held_snapshot(&f);
    let refused = f
        .runtime
        .answer_decision_request_as_human(&request.id, "dismiss", None, "system".into(), None)
        .unwrap_err()
        .to_string();
    assert!(refused.contains("answered by a human"), "{refused}");
    assert_eq!(held_snapshot(&f), before);

    answer(&f, &request, "dismiss").unwrap();
    assert_eq!(status(&f), TaskStatus::Blocked);
    assert_eq!(
        f.runtime.get_task(&f.task).unwrap().github_pr_number(),
        Some("17")
    );
    assert_eq!(recorded_by_anyone(&f, CANDIDATE_DISCARDED_EVENT), 0);
    let after = held_snapshot(&f);
    assert_eq!(after.2, JobRunState::Held, "the hold stays on record");
    assert_eq!(after.3, before.3, "no delivery was admitted");

    // A stale replay of the answered request applies nothing.
    let stale = answer(&f, &request, "dismiss").unwrap_err().to_string();
    assert!(!stale.is_empty());
    assert_eq!(held_snapshot(&f).1, after.1);

    // The named route applies: the resume continues the held run on its
    // published candidate and readmits the task to its delivery.
    let resumed = f
        .runtime
        .submit_resume_run(run, Some("operator"), None)
        .unwrap();
    let resumed = f.runtime.show_job_run_observed(&resumed.run_id).unwrap();
    assert_eq!(resumed.retry_source_run_id.as_deref(), Some(run.as_str()));
    let task = f.runtime.get_task(&f.task).unwrap();
    assert!(
        matches!(task.status, TaskStatus::InProgress | TaskStatus::Review),
        "the resume readmits the task: {}",
        task.status
    );
    assert_eq!(task.github_pr_number(), Some("17"));
    assert_eq!(recorded_by_anyone(&f, CANDIDATE_DISCARDED_EVENT), 0);
}

/// The incident's open request, raised before the fix with a requeue: the
/// requeue is refused with the resume that can apply, and nothing changes.
fn a_requeue_of_a_held_forge_observation_names_the_resume() {
    let f = held_observation_fixture();
    let request = raise_block_triage(&f);
    let run = &f.source.run_id;
    let before = held_snapshot(&f);

    let error = answer(&f, &request, "2").unwrap_err().to_string();

    assert!(
        error.contains(&format!("orbit job resume {run}")) && error.contains("--discard-candidate"),
        "the refusal names the resume and the explicit discard: {error}"
    );
    assert_eq!(held_snapshot(&f), before);
    assert_eq!(
        f.runtime
            .get_decision_request(&request.id)
            .unwrap()
            .unwrap()
            .state,
        DecisionRequestState::Open
    );
    // A rewrite that leaves the candidate in place is refused the same way.
    let error = answer(&f, &request, "1").unwrap_err().to_string();
    assert!(
        error.contains(&format!("orbit job resume {run}")),
        "{error}"
    );
    assert_eq!(held_snapshot(&f), before);
    // Discarding stays the operator's explicit selection.
    f.runtime
        .update_task_as_human(
            &f.task,
            TaskUpdateParams {
                status: Some(TaskStatus::Backlog),
                discard_candidate: true,
                ..Default::default()
            },
            "human".into(),
        )
        .unwrap();
    assert_eq!(status(&f), TaskStatus::Backlog);
    assert_eq!(recorded_by_anyone(&f, CANDIDATE_DISCARDED_EVENT), 1);
}

/// The route is offered only while its prerequisites hold: a discarded
/// candidate or a run that is still live is not resumed.
fn the_held_resume_is_not_offered_without_its_prerequisites() {
    let mut f = held_observation_fixture();
    let next_resume = |f: &Fixture| {
        let task = f.runtime.get_task(&f.task).unwrap();
        let history = f.runtime.get_task_history(&f.task).unwrap();
        f.runtime
            .blocked_next_step(&task, &history)
            .and_then(|next| next.resume_command)
    };
    let request = raise_block_triage(&f);

    park_source(&mut f, JobRunState::Running, false);
    assert_eq!(next_resume(&f), None, "a live run is not resumed");
    let error = answer(&f, &request, "2").unwrap_err().to_string();
    assert!(!error.contains("orbit job resume"), "{error}");
    assert_eq!(status(&f), TaskStatus::Blocked);

    hold_on_forge_observation(&mut f);
    orbit_core::test_support::EngineHost::new(&f.runtime)
        .apply_task_automation_update(
            &f.task,
            orbit_engine::TaskAutomationUpdate {
                append_history: vec![orbit_types::task::TaskHistoryEntry {
                    at: Utc::now(),
                    by: "system".into(),
                    event: CANDIDATE_DISCARDED_EVENT.into(),
                    note: None,
                    from_status: None,
                    to_status: None,
                    operation_id: None,
                    decision: None,
                    surface: None,
                    caller: None,
                }],
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        next_resume(&f),
        None,
        "a discarded candidate is not resumed"
    );
    let error = answer(&f, &request, "2").unwrap_err().to_string();
    assert!(!error.contains("orbit job resume"), "{error}");
    assert_eq!(status(&f), TaskStatus::Blocked);
}

/// A completion failure that is not a forge observation keeps the
/// requeue-only question its escalation asks.
fn an_ordinary_escalation_keeps_its_requeue_answer() {
    let (f, _, _) = escalated_completion_fixture();
    let request = open_request(&f);
    assert_eq!(
        request
            .answers
            .iter()
            .map(|answer| answer.label.as_str())
            .collect::<Vec<_>>(),
        ["Done — requeue"]
    );
}

crate::scenario_table!(
    held_observation_scenarios,
    a_held_forge_observation_offers_the_resume_instead_of_a_requeue,
    a_requeue_of_a_held_forge_observation_names_the_resume,
    the_held_resume_is_not_offered_without_its_prerequisites,
    an_ordinary_escalation_keeps_its_requeue_answer
);
