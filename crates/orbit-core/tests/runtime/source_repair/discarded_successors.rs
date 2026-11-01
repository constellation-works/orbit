//! [ORB-20956] An operator's discard of a publication survives attempts that
//! stopped without publishing.
//!
//! A publisher's candidate is discarded through the operator route, the next
//! attempt implements fresh because of it and stops held without a PR, and
//! the attempt after that must still implement fresh without another discard,
//! reusing neither the discarded head nor its PR. Every link of that chain is
//! proven from host checkpoints; anything else keeps the legacy-publication
//! refusal.
use super::published_owner::{checkpoint, next, save_run};
use super::*;
use orbit_core::application::task::TaskUpdateParams;
use orbit_types::task::TaskHistoryEntry;
use orbit_types::workflow::{FailureActivityCheckpoint, JobRun};

/// The fixture job's `resume_candidate` step.
const ADOPTION_STEP: u32 = 6;
const LEGACY_REFUSAL: &str = "a published candidate predates the current binding";

/// The publisher's failure handoff kept its candidate, as in the incident:
/// the next attempt's fresh outcome then names it as the discarded source.
fn keep_publisher_candidate(f: &Fixture) {
    let mut state = f.runtime.read_run_state(&f.source.run_id).unwrap().unwrap();
    state.failure_activity_checkpoint = Some(FailureActivityCheckpoint {
        activity_name: "pr_failure_handoff".into(),
        failed_step_id: "complete_pr".into(),
        output: json!({"decision":"blocked_failure_pr","task_id":f.task,
            "branch":f.branch,"head":f.head,"head_sha":f.head}),
    });
    f.runtime.write_run_state(&f.source.run_id, &state).unwrap();
}

/// The supported operator route: `orbit task update --status backlog
/// --discard-candidate`.
fn discard(f: &Fixture) {
    f.runtime
        .update_task_as_human(
            &f.task,
            TaskUpdateParams {
                status: Some(TaskStatus::Backlog),
                discard_candidate: true,
                ..Default::default()
            },
            "operator".into(),
        )
        .unwrap();
}

/// Admit the next owner attempt and run its candidate adoption with the
/// prior run its worktree setup recorded.
fn attempt(f: &Fixture) -> (JobRun, Value, Result<Value, String>) {
    f.runtime
        .apply_task_automation_update(
            &f.task,
            TaskAutomationUpdate {
                status: Some(TaskStatus::InProgress),
                ..Default::default()
            },
        )
        .unwrap();
    let (run, setup) = next(f);
    let resumed = adopt(f, &run, &setup);
    (run, setup, resumed)
}

fn adopt(f: &Fixture, run: &JobRun, setup: &Value) -> Result<Value, String> {
    execute_deterministic_action(
        &f.runtime,
        "candidate_resume",
        &json!({}),
        &json!({"job_run_id":run.run_id,"task_ids":[f.task],
            "workspace_path":setup["workspace_path"],"base_sha":setup["base_sha"],
            "prior_job_run_id":setup["prior_job_run_id"]}),
        false,
        &HashMap::new(),
        None,
    )
    .map_err(|error| error.to_string())
}

/// Checkpoint the attempt's adoption and stop it held, before any commit or
/// publication, as `crew_ownership_mismatch` stopped the incident's attempt.
fn hold(f: &Fixture, run: &mut JobRun, resumed: Value) {
    checkpoint(f, &run.run_id, ADOPTION_STEP, "resume_candidate", resumed);
    run.state = JobRunState::Failed;
    run.pid = None;
    run.finished_at = Some(Utc::now());
    save_run(f, run);
}

fn assert_fresh(resumed: &Value, case: &str) {
    assert_eq!(resumed["outcome"], "fresh", "{case}: {resumed}");
    assert_eq!(resumed["implement"], true, "{case}: {resumed}");
    assert!(resumed["reused_branch"].is_null(), "{case}: {resumed}");
    assert!(resumed["reused_head_sha"].is_null(), "{case}: {resumed}");
    assert!(resumed["prior_pull_request"].is_null(), "{case}: {resumed}");
}

fn clean(setup: &Value) -> String {
    git(
        &PathBuf::from(setup["workspace_path"].as_str().unwrap()),
        &["status", "--porcelain"],
    )
}

/// F2026-11-023: discard, a fresh held attempt, then an attempt that must
/// still implement fresh, and another after a restart and a second held
/// attempt.
pub(super) fn discard_survives_unpublished_held_successors() {
    let f = Fixture::new();
    keep_publisher_candidate(&f);
    let refs = f.runtime.get_task(&f.task).unwrap().external_refs;
    let publisher_state =
        serde_json::to_value(f.runtime.read_run_state(&f.source.run_id).unwrap()).unwrap();
    discard(&f);

    let (mut held, setup, resumed) = attempt(&f);
    assert_eq!(setup["prior_job_run_id"], f.source.run_id);
    let resumed = resumed.unwrap();
    assert_fresh(&resumed, "first successor");
    assert_eq!(resumed["reason_code"], "candidate_discarded");
    assert_eq!(resumed["source_run_id"], f.source.run_id);
    assert_eq!(resumed["source_sha"], f.head);
    hold(&f, &mut held, resumed);

    let (mut following, setup, resumed) = attempt(&f);
    assert_eq!(setup["prior_job_run_id"], held.run_id);
    let resumed = resumed
        .unwrap_or_else(|error| panic!("the discard still governs after a held attempt: {error}"));
    assert_fresh(&resumed, "following attempt");
    assert_eq!(
        clean(&setup),
        "",
        "nothing of the discarded head is applied"
    );
    // A restart replays the adoption step on the same run.
    let replayed = adopt(&f, &following, &setup).unwrap();
    assert_eq!(replayed, resumed, "a restarted adoption decides the same");
    hold(&f, &mut following, resumed);

    let (_, setup, resumed) = attempt(&f);
    assert_eq!(setup["prior_job_run_id"], following.run_id);
    assert_fresh(
        &resumed.unwrap_or_else(|error| panic!("second held attempt: {error}")),
        "after two held attempts",
    );

    let task = f.runtime.get_task(&f.task).unwrap();
    assert_eq!(task.external_refs, refs, "closed publication refs are kept");
    assert_eq!(
        serde_json::to_value(f.runtime.read_run_state(&f.source.run_id).unwrap()).unwrap(),
        publisher_state,
        "the discarded publication's evidence is untouched"
    );
    assert_eq!(
        f.runtime
            .get_task_history(&f.task)
            .unwrap()
            .iter()
            .filter(|e| e.event == orbit_types::task::CANDIDATE_DISCARDED_EVENT)
            .count(),
        1,
        "no second discard was needed"
    );
}

/// Every link the walk cannot prove keeps the legacy-publication refusal,
/// before the incoming attempt touches its checkout.
pub(super) fn discard_across_successors_keeps_every_refusal() {
    for case in [
        "no_discard",
        "successor_pushed",
        "successor_handoff_published",
        "successor_live",
        "successor_unauthenticated_output",
        "successor_resumed_candidate",
        "successor_names_foreign_source",
        "successor_other_prior",
        "publisher_live",
        "publisher_preserved",
        "newer_execution",
    ] {
        let mut f = Fixture::new();
        if case != "no_discard" {
            discard(&f);
        }
        let (mut held, setup, resumed) = attempt(&f);
        let resumed = match (case, resumed) {
            // Without a discard the first attempt already refuses the
            // undiscarded publication; stop it as a fresh attempt anyway.
            ("no_discard", Err(error)) => {
                assert!(error.contains("--discard-candidate"), "{case}: {error}");
                json!({"phase":"candidate_resume","outcome":"fresh","implement":true,
                    "reason":"fixture","reason_code":"no_candidate","repair":null,
                    "source_run_id":null,"source_machine_id":null,"source_branch":null,
                    "source_sha":null,"base_sha":setup["base_sha"],"prior_pull_request":null,
                    "prior_pull_request_head":null,"reused_branch":null,
                    "reused_head_sha":null,"branch_reuse_refused":null})
            }
            (_, resumed) => resumed.unwrap_or_else(|error| panic!("{case}: {error}")),
        };
        hold(&f, &mut held, resumed.clone());
        let mut state = f.runtime.read_run_state(&held.run_id).unwrap().unwrap();
        match case {
            "no_discard" => {}
            "successor_pushed" => {
                // A publication after the discard stays subject to adoption.
                state.record_pipeline_output(
                    "push",
                    json!({"phase":"push","local_sha":f.head,"branch":f.branch}),
                );
                f.runtime.write_run_state(&held.run_id, &state).unwrap();
            }
            "successor_handoff_published" => {
                state.failure_activity_checkpoint = Some(FailureActivityCheckpoint {
                    activity_name: "pr_failure_handoff".into(),
                    failed_step_id: "resume_candidate".into(),
                    output: json!({"decision":"blocked_failure_pr","task_id":f.task}),
                });
                f.runtime.write_run_state(&held.run_id, &state).unwrap();
            }
            "successor_live" => {
                held.state = JobRunState::Running;
                held.pid = Some(std::process::id());
                held.finished_at = None;
                save_run(&f, &held);
            }
            "successor_unauthenticated_output" => {
                // The pipeline record no longer matches the step's output.
                state.pipeline["resume_candidate"]["source_run_id"] = json!("jrun-forged");
                f.runtime.write_run_state(&held.run_id, &state).unwrap();
            }
            "successor_resumed_candidate" => {
                let mut changed = resumed.clone();
                changed["outcome"] = json!("resumed_carried");
                changed["implement"] = json!(false);
                checkpoint(&f, &held.run_id, ADOPTION_STEP, "resume_candidate", changed);
            }
            "successor_names_foreign_source" => {
                let mut changed = resumed.clone();
                changed["reason_code"] = json!("candidate_discarded");
                changed["source_run_id"] = json!("jrun-foreign");
                checkpoint(&f, &held.run_id, ADOPTION_STEP, "resume_candidate", changed);
            }
            "successor_other_prior" => {
                let mut worktree = state.pipeline["worktree"].clone();
                worktree["prior_job_run_id"] = json!("jrun-other");
                checkpoint(&f, &held.run_id, 0, "worktree", worktree);
            }
            "publisher_live" => {
                f.source.pid = Some(std::process::id());
                f.source.finished_at = None;
                f.source.state = JobRunState::Running;
                save_run(&f, &f.source);
            }
            "publisher_preserved" => {
                let note = json!({"schema_version":1,"source_run_id":f.source.run_id,
                    "task_digest":"fixture"});
                f.runtime
                    .apply_task_automation_update(
                        &f.task,
                        TaskAutomationUpdate {
                            append_history: vec![TaskHistoryEntry {
                                at: Utc::now(),
                                by: "system".into(),
                                event: "published_owner_requeued".into(),
                                note: Some(note.to_string()),
                                from_status: None,
                                to_status: None,
                                operation_id: None,
                                decision: None,
                            }],
                            ..Default::default()
                        },
                    )
                    .unwrap();
            }
            "newer_execution" => {}
            _ => unreachable!(),
        }
        f.runtime
            .apply_task_automation_update(
                &f.task,
                TaskAutomationUpdate {
                    status: Some(TaskStatus::InProgress),
                    ..Default::default()
                },
            )
            .unwrap();
        let (incoming, setup) = next(&f);
        if case == "newer_execution" {
            let mut newer = f
                .runtime
                .insert_job_run(
                    "task_pr_pipeline",
                    1,
                    Utc::now(),
                    Some(json!({"task_ids":[f.task]})),
                    None,
                )
                .unwrap();
            newer.state = JobRunState::Running;
            newer.pid = Some(std::process::id());
            save_run(&f, &newer);
        }
        let error = adopt(&f, &incoming, &setup).expect_err(case).to_string();
        // A publication after the discard is judged as the publisher by the
        // existing adoption checks; everything else keeps the legacy refusal.
        let expected = match case {
            "successor_pushed" => "lacks a unique requeue binding",
            _ => LEGACY_REFUSAL,
        };
        assert!(error.contains(expected), "{case}: {error}");
        assert_eq!(
            clean(&setup),
            "",
            "{case}: refusal precedes any checkout write"
        );
    }
}
