//! Pilot/owner-pull interleavings at the real owner tool boundary.

use super::*;

fn action(pair: &Pair, name: &str, input: Value) -> Value {
    pair.wire
        .owner
        .run_deterministic(name, &json!({}), &input, ToolContext::default())
        .unwrap()
}

fn prepare(pair: &Pair, task_id: &str) -> Value {
    action(
        pair,
        "prepare_task_pilot",
        json!({
            "workspace_path": pair.owner_repo, "task_ids": [task_id], "base_branch": "main",
        }),
    )
}

#[test]
fn owner_pull_defers_an_active_pilot_and_admits_it_after_the_hold_ends() {
    if !isolated(
        module_path!(),
        "owner_pull_defers_an_active_pilot_and_admits_it_after_the_hold_ends",
    ) {
        return;
    }
    let pair = Pair::new(2);
    let prepared = prepare(&pair, &pair.tasks[0]);
    let jobs = orbit_store::compose::workspace_job_run_store(
        pair.wire.owner.sqlite_store().unwrap(),
        pair.wire.owner.workspace_id().unwrap(),
    );
    let run = jobs
        .insert_job_run("task_pilot_pipeline", 1, Utc::now(), None, None)
        .unwrap();
    jobs.mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
        .unwrap();
    let mut state = PipelineState::new(run.run_id.clone(), run.job_id, json!({}));
    state.record_step(0, JobRunState::Success, Some(prepared), None);
    pair.wire
        .owner
        .write_run_state(&run.run_id, &state)
        .unwrap();

    let drain = pair.start_drain();
    let first = pair.pass(&drain);
    assert!(launch_refused(&first), "{first}");
    assert_eq!(pair.owner_status(&pair.tasks[0]), "backlog");
    assert_eq!(pair.owner_claims()[0]["claim"]["task_id"], pair.tasks[1]);
    let records = pair.follower_jobs.local_pull_admissions().unwrap();
    let receipt = records
        .iter()
        .filter_map(|record| record.receipt.as_ref())
        .find(|receipt| {
            receipt
                .claim
                .as_ref()
                .is_some_and(|claim| claim.task_id == pair.tasks[1])
        })
        .unwrap();
    assert_eq!(
        receipt.queue_depth, 0,
        "the remaining queue excludes the piloted task"
    );
    assert!(
        receipt.deferred_conflicts.iter().any(|entry| {
            entry.task_id == pair.tasks[0] && entry.reason.contains("active task-pilot preparation")
        }),
        "{receipt:?}"
    );

    jobs.finalize_job_run(&run.run_id, JobRunState::Success, Utc::now(), None)
        .unwrap();
    let second = pair.pass(&drain);
    assert!(launch_refused(&second), "{second}");
    assert!(
        pair.owner_claims()
            .iter()
            .any(|entry| entry["claim"]["task_id"] == pair.tasks[0])
    );
}

#[test]
fn follower_claim_supersedes_a_prepared_assessment_without_unscoped_writes() {
    if !isolated(
        module_path!(),
        "follower_claim_supersedes_a_prepared_assessment_without_unscoped_writes",
    ) {
        return;
    }
    let pair = Pair::new(1);
    let prepared = prepare(&pair, &pair.tasks[0]);
    let drain = pair.start_drain();
    let leaf = pair.queued_leaf(&drain, 1);
    assert_eq!(pair.claimed_task(&leaf), pair.tasks[0]);
    let before = pair.owner_task(&pair.tasks[0]);
    let output = action(
        &pair,
        "apply_task_pilot_results",
        json!({
            "workspace_path": pair.owner_repo, "prepared": prepared,
            "results": [{"partition_index": 0, "task_ids": pair.tasks,
                "tasks": [{"task_id": pair.tasks[0], "context_files_before": ["file:src/f0.rs"]}]}],
        }),
    );
    assert_eq!(output["status"], "succeeded", "{output}");
    assert_eq!(output["outcome"], "superseded");
    assert_eq!(output["task_outcomes"][0]["reason"], "execution_claim");
    assert_eq!(output["applied_count"], 0);
    assert_eq!(output["unresolved_count"], 0);
    assert_eq!(pair.owner_task(&pair.tasks[0]), before);
    assert_eq!(
        action(&pair, "pipeline_success_guard", json!({"result": output}))["succeeded"],
        true
    );
}
