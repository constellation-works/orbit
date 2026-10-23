//! [ORB-16379] An unavailable provider is the failing host's: a hold placed
//! because the owner's login failed keeps the task off that crew on the
//! owner, while a follower whose login works is still offered it there.

use orbit_engine::TaskAutomationUpdate;
use orbit_types::task::TaskStatus;
use orbit_types::workflow::{
    PROVIDER_FAILURE_HOLD_EVENT, ProviderFailureClass, ProviderFailureHold, provider_failure_text,
};

use super::*;

const CREWS: &str = "\
[workflow]
default_crew = \"sol\"

[crews.sol]
provider = \"codex\"
model = \"gpt-sol\"

[crews.luna]
provider = \"codex\"
model = \"gpt-luna\"

[crews.opus]
provider = \"claude\"
model = \"claude-opus\"
";

/// The owner's own local run of `task` as `crew` fails with `class`, the
/// way its worker records the step and finalizes the run.
fn owner_run_fails(pair: &Pair, task: &str, crew: &str, class: ProviderFailureClass) {
    let owner = &pair.wire.owner;
    let jobs = orbit_store::compose::workspace_job_run_store(
        owner.sqlite_store().unwrap(),
        owner.workspace_id().unwrap(),
    );
    let input = json!({ "task_ids": [task], "crew": crew });
    let now = Utc::now();
    let run = jobs
        .insert_job_run("task_local_pipeline", 1, now, Some(input.clone()), None)
        .unwrap();
    jobs.mark_job_run_running(&run.run_id, now, std::process::id())
        .unwrap();
    jobs.write_run_state(
        &run.run_id,
        &PipelineState::new(run.run_id.clone(), run.job_id, input),
    )
    .unwrap();
    owner
        .apply_task_automation_update(
            task,
            TaskAutomationUpdate {
                status: Some(TaskStatus::InProgress),
                job_run_id: Some(run.run_id.clone()),
                ..Default::default()
            },
        )
        .unwrap();
    owner
        .complete_job_run_step(
            &run.run_id,
            &JobRunStepParams {
                lock_wait_ms: None,
                step_index: 1,
                target_type: JobTargetType::Activity,
                target_id: "implement_one".into(),
                started_at: now,
                finished_at: now,
                duration_ms: Some(1),
                exit_code: Some(1),
                agent_response_json: None,
                state: JobRunState::Failed,
                error_code: Some("STEP_FAILED".into()),
                error_message: Some(format!(
                    "step `implement_one`: {}",
                    provider_failure_text(class, "codex", "OAuth session expired")
                )),
            },
        )
        .unwrap();
    owner
        .finalize_job_run(&run.run_id, JobRunState::Failed, now, Some(1))
        .unwrap();
}

/// The owner's capacity failure on `luna` excludes it on every machine; its
/// login then fails on `sol`, which is excluded only on the owner. The owner
/// waits the task out, and the follower is offered it on `sol`.
fn an_owners_failed_login_does_not_keep_the_task_from_a_follower() {
    let pair = Pair::with_configs(CREWS, CREWS, &[Some("sol")]);
    let task = pair.tasks[0].clone();
    owner_run_fails(&pair, &task, "luna", ProviderFailureClass::Capacity);
    owner_run_fails(&pair, &task, "sol", ProviderFailureClass::Unavailable);

    let history = pair.wire.owner.get_task_history(&task).unwrap();
    let entry = history.last().expect("history");
    assert_eq!(entry.event, PROVIDER_FAILURE_HOLD_EVENT, "{entry:?}");
    let hold = ProviderFailureHold::from_text(entry.note.as_deref().unwrap_or_default())
        .expect("the note carries the hold");
    assert_eq!(hold.class, ProviderFailureClass::Unavailable, "{hold:?}");
    assert_eq!(hold.machine.as_deref(), Some(OWNER), "{hold:?}");
    assert_eq!(hold.excluded_crews, ["sol"], "only on the owner: {hold:?}");
    assert_eq!(
        hold.carried_crews,
        ["luna"],
        "capacity stays excluded on every machine: {hold:?}"
    );

    // On the owner, whose login failed, the task waits.
    let backlog = pair
        .wire
        .owner
        .run_deterministic(
            "list_backlog_tasks",
            &json!({}),
            &json!({}),
            ToolContext::default(),
        )
        .unwrap();
    let excluded = backlog["excluded"]
        .as_array()
        .and_then(|excluded| excluded.iter().find(|entry| entry["id"] == task.as_str()))
        .unwrap_or_else(|| panic!("the owner waits the task out: {backlog:#}"));
    assert_eq!(excluded["reason"], "provider_backoff", "{excluded}");

    // The follower's login works: its drain is offered the task on `sol`.
    let drain = pair.run_drain();
    let pass = pair.pass(&drain);
    assert!(
        pair.owner_has_claim_for_task(&task),
        "the follower claims the task: {pass:#}"
    );
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    an_owners_failed_login_does_not_keep_the_task_from_a_follower_scenarios,
    an_owners_failed_login_does_not_keep_the_task_from_a_follower
);
