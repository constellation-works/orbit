use super::*;
use orbit_store::contracts::JobRunStepParams;
use orbit_types::workflow::{DeliveryRefusal, JobTargetType};

fn held_completion_fixture() -> Fixture {
    let mut f = Fixture::new();
    f.source.state = JobRunState::Held;
    f.runtime
        .sqlite_store()
        .unwrap()
        .upsert_job_run_for_workspace(&f.runtime.workspace_id().unwrap(), &f.source, None)
        .unwrap();
    // Completion failed after publication. The worker leaves the last
    // successful checkpoints intact and appends a run-level diagnostic.
    f.runtime
        .complete_job_run_step(
            &f.source.run_id,
            &JobRunStepParams {
                step_index: 6,
                target_type: JobTargetType::Job,
                target_id: f.source.job_id.clone(),
                state: JobRunState::Held,
                started_at: f.source.created_at,
                finished_at: f.source.finished_at.unwrap(),
                duration_ms: None,
                exit_code: None,
                lock_wait_ms: None,
                agent_response_json: None,
                error_code: Some(
                    DeliveryRefusal::CheckEvidenceInconclusive
                        .error_code()
                        .into(),
                ),
                error_message: Some(
                    "auto_fix_stopped: required-check evidence is unavailable".into(),
                ),
            },
        )
        .unwrap();
    f
}

fn published_completion_hold_starts_repair_at_published_head() {
    let f = held_completion_fixture();
    let binding = f.inspect();
    assert_eq!(binding["head_sha"], f.head);
    f.runtime
        .apply_task_automation_update(
            &f.task,
            TaskAutomationUpdate {
                status: Some(TaskStatus::Blocked),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(f.inspect(), binding);
    let admitted = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", f.submit(binding))
        .unwrap();
    let id = admitted["run_id"].as_str().unwrap();
    assert_eq!(admitted["retry_source_run_id"], f.source.run_id);
    assert_eq!(admitted["completion"], "review");
    f.wait(id);
    f.start_actions(id);
    let setup = f.action(
        "worktree_setup",
        json!({"job_run_id":id,"task_ids":[f.task],"base":"main","base_sync":"remote"}),
    );
    let checkout = PathBuf::from(setup["workspace_path"].as_str().unwrap());
    let resumed = f.action(
        "candidate_resume",
        json!({"job_run_id":id,"task_ids":[f.task],"workspace_path":checkout,
            "base_sha":setup["base_sha"],"prior_job_run_id":f.source.run_id}),
    );
    assert_eq!(resumed["implement"], true);
    assert_eq!(resumed["reused_branch"], f.branch);
    assert_eq!(resumed["source_sha"], f.head);
    assert_eq!(resumed["reused_head_sha"], f.head);
    assert_eq!(resumed["prior_pull_request"], "17");
    assert_eq!(git(&checkout, &["branch", "--show-current"]), f.branch);
    // Source repair reapplies the published changes as uncommitted edits on
    // the current base. In this fixture the base is unchanged.
    git(&checkout, &["diff", "--exit-code", &f.head, "--"]);
    f.assert_original();
}

fn unpublished_held_run_cannot_authorize_repair() {
    let f = held_completion_fixture();
    git(&f.repo, &["push", "origin", "--delete", &f.branch]);
    let mut state = f.state.clone();
    let outputs = state.pipeline.as_object_mut().unwrap();
    outputs.remove("push");
    outputs.remove("pr_open");
    f.runtime.write_run_state(&f.source.run_id, &state).unwrap();
    f.replace_refs(Vec::new());
    let task = f.runtime.get_task(&f.task).unwrap();
    let history = f.runtime.get_task_history(&f.task).unwrap();
    let error = f
        .runtime
        .run_tool_as_human("orbit.workflow.run.resume", f.request("inspect"))
        .unwrap_err();
    assert!(
        error.to_string().contains("source lacks push evidence"),
        "{error}"
    );
    assert_eq!(f.runtime.get_task(&f.task).unwrap(), task);
    assert_eq!(f.runtime.get_task_history(&f.task).unwrap(), history);
}

crate::scenario_table!(
    held_publication_source_repair,
    published_completion_hold_starts_repair_at_published_head,
    unpublished_held_run_cannot_authorize_repair,
);
