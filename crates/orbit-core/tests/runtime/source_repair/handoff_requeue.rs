//! A task blocked by a failure handoff that published its candidate is
//! requeued onto that candidate — by an operator, or by the cleared-infra
//! requeue of a transient infrastructure block — and its next run adopts the
//! same branch, head and pull request without re-implementing. The
//! publication's authentication refusals still refuse, and a changed task
//! specification is refused with an explicit discard-and-rerun exit.
use super::failed_publication::escalated_handoff_fixture;
use super::published_owner::{checkpoint, next, save_run};
use super::*;
use orbit_types::task::TaskHistoryEntry;
use orbit_types::workflow::{
    TRANSIENT_INFRA_REQUEUED_EVENT, TransientInfraRequeue, TransientInfrastructure,
};

fn requeue(f: &Fixture, discard: bool) -> Result<Value, orbit_core::OrbitError> {
    f.runtime.run_tool_as_human(
        "orbit.task.requeue",
        json!({"id": f.task, "workspace": f.repo, "reason": "the publication is sound; resume it",
            "discard_candidate": discard}),
    )
}

fn status(f: &Fixture) -> TaskStatus {
    f.runtime.get_task(&f.task).unwrap().status
}

/// The index of step `id` in the fixture's pipeline definition.
fn step_index(f: &Fixture, id: &str) -> u32 {
    let path = f
        ._root
        .path()
        .join("global/resources/jobs/task_pr_pipeline.yaml");
    let job: Value = serde_yaml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    job["spec"]["steps"]
        .as_array()
        .unwrap()
        .iter()
        .position(|step| step["id"] == id)
        .unwrap() as u32
}

/// The next run adopts the published candidate and carries it, unchanged,
/// through commit, validation, push and the same pull request.
fn assert_next_run_resumes_the_published_candidate(f: &Fixture) {
    let before = serde_json::to_value(f.runtime.read_run_state(&f.source.run_id).unwrap()).unwrap();
    let (run, setup) = next(f);
    let checkout = PathBuf::from(setup["workspace_path"].as_str().unwrap());
    let resumed = f.action(
        "candidate_resume",
        json!({"job_run_id":run.run_id,"task_ids":[f.task],
        "workspace_path":checkout,"base_sha":setup["base_sha"]}),
    );
    assert_eq!(resumed["outcome"], "resumed_carried", "{resumed}");
    assert_eq!(resumed["implement"], false, "no new implementation");
    assert_eq!(resumed["source_run_id"], f.source.run_id);
    assert_eq!(resumed["reused_branch"], f.branch, "no new branch");
    assert_eq!(resumed["reused_head_sha"], f.head);
    assert_eq!(resumed["prior_pull_request"], "17");
    checkpoint(
        f,
        &run.run_id,
        step_index(f, "resume_candidate"),
        "resume_candidate",
        resumed,
    );
    f.action(
        "git_commit",
        json!({"job_run_id":run.run_id,"scope":"all","workspace_path":checkout,
        "base_sha":setup["base_sha"],"base_ref":setup["base_ref"]}),
    );
    let validated = f.action(
        "candidate_validate",
        json!({"job_run_id":run.run_id,"completed_task_ids":[f.task],
        "workspace_path":checkout,"base_sha":setup["base_sha"],"base_ref":setup["base_ref"]}),
    );
    assert_eq!(validated["decision"], "passed", "validation runs afresh");
    let pushed = f.action(
        "git_push",
        json!({"job_run_id":run.run_id,"workspace_path":checkout,"branch":f.branch,
        "base":"main","base_ref":setup["base_ref"]}),
    );
    assert_eq!(pushed["branch"], f.branch);
    assert_eq!(pushed["remote_sha_before"], f.head);
    let mut answer: Value = serde_json::from_slice(&std::fs::read(&f.answer).unwrap()).unwrap();
    answer["headRefOid"] = json!(git(&checkout, &["rev-parse", "HEAD"]));
    std::fs::write(&f.answer, answer.to_string()).unwrap();
    let opened = f.action(
        "pr_open",
        json!({"job_run_id":run.run_id,"workspace_path":checkout,"completed_task_ids":[f.task],
        "head":f.branch,"base":"main","base_ref":setup["base_ref"],"base_sha":setup["base_sha"]}),
    );
    assert_eq!(opened["pr_reused"], true, "no new pull request");
    assert_eq!(opened["pr_number"], "17");
    assert_eq!(
        serde_json::to_value(f.runtime.read_run_state(&f.source.run_id).unwrap()).unwrap(),
        before,
        "the stopped run's receipts are immutable"
    );
}

/// A refused requeue leaves the task, its history and the run's receipts as
/// they were.
fn refused(f: &Fixture, discard: bool, case: &str) -> String {
    let task = serde_json::to_value(f.runtime.get_task(&f.task).unwrap()).unwrap();
    let history = serde_json::to_value(f.runtime.get_task_history(&f.task).unwrap()).unwrap();
    let state = serde_json::to_value(f.runtime.read_run_state(&f.source.run_id).unwrap()).unwrap();
    let error = requeue(f, discard)
        .err()
        .unwrap_or_else(|| panic!("{case}: unexpectedly requeued"));
    assert_eq!(
        serde_json::to_value(f.runtime.get_task(&f.task).unwrap()).unwrap(),
        task,
        "{case}: task changed"
    );
    assert_eq!(
        serde_json::to_value(f.runtime.get_task_history(&f.task).unwrap()).unwrap(),
        history,
        "{case}: history written"
    );
    assert_eq!(
        serde_json::to_value(f.runtime.read_run_state(&f.source.run_id).unwrap()).unwrap(),
        state,
        "{case}: receipt rewritten"
    );
    error.to_string()
}

/// The incident: a blocked failure-handoff task could only be archived and
/// refiled. An operator requeue now resumes its published candidate.
fn operator_requeue_resumes_the_failure_handoff_candidate_and_pull_request() {
    let (f, _) = escalated_handoff_fixture(None, None);
    assert_eq!(status(&f), TaskStatus::Blocked);
    assert!(
        f.runtime
            .transient_infra_blocked_tasks()
            .unwrap()
            .is_empty(),
        "an agent's escalation is not an infra block"
    );
    let answer = requeue(&f, false).unwrap();
    assert_eq!(answer["status"], "backlog");
    assert_eq!(answer["source_run_id"], f.source.run_id);
    assert_eq!(answer["resumes"]["pull_request"], "17");
    assert_eq!(answer["resumes"]["branch"], f.branch);
    assert_eq!(answer["resumes"]["head_sha"], f.head);
    assert_eq!(answer["candidate_discarded"], false);
    // Only a blocked task is requeued; a repeat writes nothing.
    let history = f.runtime.get_task_history(&f.task).unwrap().len();
    assert!(
        requeue(&f, false).is_err(),
        "a backlog task is not requeued"
    );
    assert_eq!(f.runtime.get_task_history(&f.task).unwrap().len(), history);
    assert_next_run_resumes_the_published_candidate(&f);
}

/// Live, foreign and newer executions still refuse the publication, at the
/// requeue and with nothing written.
fn requeue_keeps_the_published_owner_refusals() {
    let (f, _) = escalated_handoff_fixture(None, None);
    let stopped = f.runtime.show_job_run_observed(&f.source.run_id).unwrap();
    let set_origin = |run: &orbit_types::workflow::JobRun| {
        // Ordinary upsert preserves the trusted execution origin; model hostile
        // persisted evidence explicitly in this isolated fixture.
        f.runtime
            .sqlite_store()
            .unwrap()
            .with_transaction(|tx| {
                tx.connection()
                    .execute(
                        "UPDATE job_runs SET executed_on_json = ?1 WHERE run_id = ?2",
                        [
                            serde_json::to_string(&run.executed_on).unwrap(),
                            run.run_id.clone(),
                        ],
                    )
                    .unwrap();
                Ok(())
            })
            .unwrap();
    };
    let mut live = stopped.clone();
    live.state = JobRunState::Running;
    live.pid = Some(std::process::id());
    live.finished_at = None;
    save_run(&f, &live);
    refused(&f, false, "live");
    save_run(&f, &stopped);
    let mut foreign = stopped.clone();
    foreign.executed_on.as_mut().unwrap().machine_id = "hm_other".into();
    set_origin(&foreign);
    refused(&f, false, "foreign");
    set_origin(&stopped);
    let newer = f
        .runtime
        .insert_job_run(
            "task_pr_pipeline",
            1,
            Utc::now(),
            Some(json!({"task_ids":[f.task]})),
            None,
        )
        .unwrap();
    assert_eq!(newer.job_id, "task_pr_pipeline");
    refused(&f, false, "newer execution");
    assert_eq!(status(&f), TaskStatus::Blocked);
}

/// A discarded candidate is never resumed, and a changed specification is
/// reported with discard-and-rerun as its exit, which then implements fresh.
fn changed_or_discarded_candidates_are_refused_with_a_discard_exit() {
    let (f, _) = escalated_handoff_fixture(None, None);
    f.runtime
        .update_task_as_human(
            &f.task,
            orbit_core::application::task::TaskUpdateParams {
                discard_candidate: true,
                ..Default::default()
            },
            "human".into(),
        )
        .unwrap();
    refused(&f, false, "discarded");
    f.runtime
        .update_task_as_human(
            &f.task,
            orbit_core::application::task::TaskUpdateParams {
                description: Some("Repair the remaining compiler error and its sibling".into()),
                ..Default::default()
            },
            "human".into(),
        )
        .unwrap();
    let error = refused(&f, false, "changed specification");
    assert!(
        error.contains("--discard-candidate"),
        "the mismatch offers its exit: {error}"
    );
    let answer = requeue(&f, true).unwrap();
    assert_eq!(answer["status"], "backlog");
    assert_eq!(answer["candidate_discarded"], true);
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
    assert_ne!(resumed["outcome"], "resumed_carried");
}

/// A delivery step that lost to a locked store blocks the task on transient
/// infrastructure; once the probe passes, the cleared-infra requeue resumes
/// the candidate its failure handoff published.
fn a_transient_infra_block_is_requeued_onto_its_candidate_after_the_probe() {
    let (f, _) = escalated_handoff_fixture(None, Some(TransientInfrastructure::StoreLocked));
    assert_eq!(status(&f), TaskStatus::Blocked);
    let blocked = f.runtime.transient_infra_blocked_tasks().unwrap();
    assert_eq!(blocked.len(), 1, "{blocked:?}");
    assert_eq!(blocked[0].class, TransientInfrastructure::StoreLocked);
    assert_eq!(blocked[0].pull_request.as_deref(), Some("17"));
    assert!(blocked[0].cleared, "the store takes a write now");
    assert!(!blocked[0].exhausted);
    let requeued = f.runtime.requeue_cleared_transient_infra_blocks().unwrap();
    assert_eq!(requeued.len(), 1);
    assert_eq!(status(&f), TaskStatus::Backlog);
    assert!(
        f.runtime
            .transient_infra_blocked_tasks()
            .unwrap()
            .is_empty(),
        "the block is spent"
    );
    assert!(
        f.runtime
            .requeue_cleared_transient_infra_blocks()
            .unwrap()
            .is_empty(),
        "a repeat requeues nothing"
    );
    assert_next_run_resumes_the_published_candidate(&f);
}

/// A publication that already took its automatic requeues stays blocked for
/// an operator, who can still requeue it.
fn transient_requeues_are_bounded_per_publication() {
    let (f, _) = escalated_handoff_fixture(None, Some(TransientInfrastructure::LockBusy));
    let spent = TaskHistoryEntry {
        at: Utc::now(),
        by: "system".into(),
        event: TRANSIENT_INFRA_REQUEUED_EVENT.into(),
        note: Some(
            serde_json::to_string(&TransientInfraRequeue {
                schema_version: 1,
                run_id: "jrun-earlier".into(),
                class: TransientInfrastructure::LockBusy,
                pull_request: Some("17".into()),
            })
            .unwrap(),
        ),
        from_status: None,
        to_status: None,
        operation_id: None,
        decision: None,
    };
    f.runtime
        .apply_task_automation_update(
            &f.task,
            TaskAutomationUpdate {
                append_history: vec![spent.clone(), spent],
                ..Default::default()
            },
        )
        .unwrap();
    let blocked = f.runtime.transient_infra_blocked_tasks().unwrap();
    assert_eq!(blocked.len(), 1);
    assert_eq!(blocked[0].requeues, 2);
    assert!(blocked[0].exhausted);
    assert!(
        f.runtime
            .requeue_cleared_transient_infra_blocks()
            .unwrap()
            .is_empty()
    );
    assert_eq!(status(&f), TaskStatus::Blocked, "the bound holds");
    assert_eq!(requeue(&f, false).unwrap()["resumes"]["pull_request"], "17");
}

crate::scenario_table!(
    handoff_requeue_scenarios,
    operator_requeue_resumes_the_failure_handoff_candidate_and_pull_request,
    requeue_keeps_the_published_owner_refusals,
    changed_or_discarded_candidates_are_refused_with_a_discard_exit,
    a_transient_infra_block_is_requeued_onto_its_candidate_after_the_probe,
    transient_requeues_are_bounded_per_publication
);
