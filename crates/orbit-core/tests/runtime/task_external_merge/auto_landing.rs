//! The owner merges a delivered pull request its authorised run left behind
//! [ORB-25966]: a `completion: done` run that ended held or without merging,
//! whose checks then went green with no decision open, lands on the owner's
//! next pass. Anything else carries a typed follow-up instead of sitting.

use orbit_core::application::task::{LANDING_FOLLOW_UP_EVENT, LandingFollowUp};
use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
use orbit_store::contracts::JobRunStepParams;
use orbit_types::task::TaskHistoryEntry;
use orbit_types::workflow::handoff::{
    LANDING_CHECKS_OBSERVED_EVENT, LandingChecksObservationState, RecordedLandingChecksObservation,
};
use orbit_types::workflow::{JobRunState, JobTargetType};

use super::*;

crate::scenario_table!(
    auto_landing,
    a_held_authorised_run_whose_checks_went_green_is_merged_by_the_owner_tick,
    a_held_absent_runs_delivery_is_not_merged_until_a_required_check_passes,
    a_succeeded_authorised_run_with_a_passed_observation_is_merged,
    a_delivery_without_completion_authority_records_who_merges_it,
    an_authorised_conflicting_pull_request_records_its_follow_up_once,
    two_open_delivery_pull_requests_record_one_ambiguity_follow_up
);

/// A finished delivery run of `task` that carried `completion`.
fn delivery_run(
    fx: &Fixture,
    task: &str,
    completion: &str,
    state: JobRunState,
    code: Option<&str>,
) {
    let jobs = orbit_store::compose::workspace_job_run_store(
        fx.runtime.sqlite_store().unwrap(),
        fx.runtime.workspace_id().unwrap(),
    );
    let run = jobs
        .insert_job_run(
            "task_pr_pipeline",
            1,
            Utc::now(),
            Some(json!({ "task_ids": [task], "completion": completion })),
            None,
        )
        .unwrap();
    jobs.mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
        .unwrap();
    jobs.complete_job_run_step(
        &run.run_id,
        &JobRunStepParams {
            step_index: 0,
            target_type: JobTargetType::Activity,
            target_id: "pr_complete".into(),
            started_at: Utc::now(),
            finished_at: Utc::now(),
            duration_ms: Some(1),
            exit_code: None,
            agent_response_json: None,
            state,
            lock_wait_ms: None,
            error_code: code.map(str::to_string),
            error_message: None,
        },
    )
    .unwrap();
    jobs.finalize_job_run(&run.run_id, state, Utc::now(), Some(1))
        .unwrap();
}

/// The watch's `passed` observation of the scripted pull request.
fn observe_passed(fx: &Fixture, task: &str) {
    let note = RecordedLandingChecksObservation::settled(
        LandingChecksObservationState::Passed,
        LAND_PR.to_string(),
        Some(LAND_HEAD.to_string()),
        None,
    )
    .to_note()
    .unwrap();
    fx.runtime
        .apply_task_automation_update(
            task,
            TaskAutomationUpdate {
                append_history: vec![
                    TaskHistoryEntry::new(
                        Utc::now(),
                        "owner-landing",
                        LANDING_CHECKS_OBSERVED_EVENT,
                    )
                    .with_note(Some(note)),
                ],
                ..TaskAutomationUpdate::default()
            },
        )
        .unwrap();
}

fn follow_ups(fx: &Fixture, task: &str) -> Vec<LandingFollowUp> {
    fx.runtime
        .get_task_history(task)
        .unwrap()
        .iter()
        .filter(|entry| entry.event == LANDING_FOLLOW_UP_EVENT)
        .map(|entry| serde_json::from_str(entry.note.as_deref().unwrap()).unwrap())
        .collect()
}

fn a_held_authorised_run_whose_checks_went_green_is_merged_by_the_owner_tick() {
    // ORB-24422: the run held after its checks retrigger; the checks passed later.
    let (fx, forge, task) = mergeability_land(&[], MergeabilityAnswer::Mergeable);
    delivery_run(
        &fx,
        &task,
        "done",
        JobRunState::Held,
        Some("delivery_check_runs_absent"),
    );
    tick(&fx, false, false);
    assert_eq!(
        forge.merges(),
        vec![(LAND_PR, LAND_HEAD.to_string())],
        "the sweep tick merges the green pull request once"
    );
    fx.assert_done(&task, "the owner settles the merged task");
    assert!(
        follow_ups(&fx, &task).is_empty(),
        "a merged task owes nothing"
    );
}

fn a_held_absent_runs_delivery_is_not_merged_until_a_required_check_passes() {
    // ORB-25993: a mergeable head with no required check run has no check
    // evidence; the sweep must not ask the forge to merge it.
    let (fx, forge, task) = mergeability_land(&[], MergeabilityAnswer::Mergeable);
    delivery_run(
        &fx,
        &task,
        "done",
        JobRunState::Held,
        Some("delivery_check_runs_absent"),
    );
    forge.report_no_required_checks(true);
    for _ in 0..2 {
        tick(&fx, false, false);
    }
    assert!(
        forge.merges().is_empty(),
        "no required check has passed, so the sweep makes no merge call"
    );
    fx.assert_status(&task, TaskStatus::Review, "waits for check evidence");
    forge.report_no_required_checks(false);
    tick(&fx, false, false);
    assert_eq!(
        forge.merges(),
        vec![(LAND_PR, LAND_HEAD.to_string())],
        "the same delivery merges once a required check passes"
    );
    fx.assert_done(&task, "the owner settles the merged task");
}

fn a_succeeded_authorised_run_with_a_passed_observation_is_merged() {
    // ORB-25385: checks observed passed, no run left to merge.
    let (fx, forge, task) = mergeability_land(&[], MergeabilityAnswer::Mergeable);
    delivery_run(&fx, &task, "done", JobRunState::Success, None);
    observe_passed(&fx, &task);
    let landed = fx.runtime.land_completion_authorized_tasks().unwrap();
    assert_eq!(landed.len(), 1, "{landed:?}");
    assert_eq!(
        (landed[0].pull_request, landed[0].task_id.as_str()),
        (LAND_PR, task.as_str())
    );
    assert_eq!(forge.merges().len(), 1);
    fx.assert_done(&task, "landed and settled");
}

fn a_delivery_without_completion_authority_records_who_merges_it() {
    let (fx, forge, task) = mergeability_land(&[], MergeabilityAnswer::Mergeable);
    delivery_run(&fx, &task, "review", JobRunState::Success, None);
    observe_passed(&fx, &task);
    for _ in 0..2 {
        assert!(
            fx.runtime
                .land_completion_authorized_tasks()
                .unwrap()
                .is_empty()
        );
    }
    assert!(forge.merges().is_empty(), "no authority, no merge");
    fx.assert_status(&task, TaskStatus::Review, "left for the operator");
    let recorded = follow_ups(&fx, &task);
    assert_eq!(recorded.len(), 1, "recorded once: {recorded:?}");
    assert_eq!(
        (
            recorded[0].code.as_str(),
            recorded[0].owner.as_str(),
            recorded[0].pull_request
        ),
        ("completion_not_authorized", "operator", LAND_PR)
    );
}

fn an_authorised_conflicting_pull_request_records_its_follow_up_once() {
    let (fx, forge, task) = mergeability_land(&[], MergeabilityAnswer::Conflicting);
    delivery_run(&fx, &task, "done", JobRunState::Success, None);
    for _ in 0..2 {
        assert!(
            fx.runtime
                .land_completion_authorized_tasks()
                .unwrap()
                .is_empty()
        );
    }
    assert!(forge.merges().is_empty(), "a conflict never merges");
    fx.assert_status(&task, TaskStatus::Review, "conflict stays review");
    let recorded = follow_ups(&fx, &task);
    assert_eq!(recorded.len(), 1, "recorded once: {recorded:?}");
    assert_eq!(recorded[0].code, "pull_request_conflicting");
}

fn two_open_delivery_pull_requests_record_one_ambiguity_follow_up() {
    // ORB-25997: two open pull requests deliver the task, so the sweep must not
    // pick one, and must leave the operator a reason instead of only logging.
    const SECOND_PR: u64 = LAND_PR + 1;
    let fx = Fixture::new();
    let task = fx.add_task("Ambiguous land", TaskStatus::Review, &[LAND_PR, SECOND_PR]);
    fx.forge
        .script_land(&task, &fx.base.clone(), &[], MergeabilityAnswer::Mergeable);
    fx.forge.0.lock().unwrap().open.insert(
        SECOND_PR,
        ForgePullRequest {
            number: SECOND_PR,
            head_branch: format!("orbit/{task}-5a5a5a5a"),
            body: format!("Delivers {task}.\n\nOrbit run jrun-2"),
            author: BOT.into(),
        },
    );
    delivery_run(&fx, &task, "done", JobRunState::Success, None);
    for _ in 0..2 {
        assert!(
            fx.runtime
                .land_completion_authorized_tasks()
                .unwrap()
                .is_empty()
        );
    }
    assert!(
        fx.forge.merges().is_empty(),
        "an ambiguous delivery never merges"
    );
    fx.assert_status(&task, TaskStatus::Review, "left for the operator");
    let recorded = follow_ups(&fx, &task);
    assert_eq!(recorded.len(), 1, "recorded once: {recorded:?}");
    assert_eq!(recorded[0].code, "ambiguous_delivery_pull_requests");
    assert_eq!(recorded[0].owner, "operator");
    assert!(
        recorded[0].detail.contains(&LAND_PR.to_string())
            && recorded[0].detail.contains(&SECOND_PR.to_string()),
        "names both pull requests: {}",
        recorded[0].detail
    );
}
