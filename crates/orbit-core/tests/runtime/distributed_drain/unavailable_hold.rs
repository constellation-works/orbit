//! [ORB-16323] An unavailable hold records the machine whose login failed.
//! Pull admission on that machine still defers the task; a different machine
//! is not excluded by it.

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
model = \"sol-model\"
";

/// The owner's own run fails unavailable. A pull whose session is that
/// machine still defers the task. A follower's pull admits it, which this
/// binary can only show by the leaf reaching a refused launch.
///
/// The follower drain is not used for the failing machine: its persisted
/// destination is the follower, so a pass with a local session would reject
/// the receipt. The owner's `orbit.task.pull` is the same admission the
/// drain calls, spoken as this host.
#[test]
fn an_unavailable_hold_excludes_only_the_machine_whose_login_failed() {
    if !isolated(
        module_path!(),
        "an_unavailable_hold_excludes_only_the_machine_whose_login_failed",
    ) {
        return;
    }
    let pair = Pair::with_configs(CREWS, CREWS, &[Some("sol")]);
    let task = pair.tasks[0].clone();
    let run = fail_owner_unavailable(&pair, &task);

    let history = pair.wire.owner.get_task_history(&task).unwrap();
    let entry = history.last().expect("history");
    assert_eq!(entry.event, PROVIDER_FAILURE_HOLD_EVENT, "{entry:?}");
    let hold = ProviderFailureHold::from_text(entry.note.as_deref().unwrap())
        .expect("the note carries the hold");
    assert_eq!(hold.class, ProviderFailureClass::Unavailable);
    assert_eq!(hold.machine.as_deref(), Some(OWNER), "{hold:?}");
    assert!(hold.excludes("sol"), "{hold:?}");
    assert_eq!(hold.run_id, run);

    *pair.wire.local.lock().unwrap() = true;
    let probe = pair
        .wire
        .call("", "orbit.drain.probe", json!({}))
        .expect("the failing machine probes as itself");
    let fingerprint = orbit_store::contracts::distributed_drain_protocol_fingerprint();
    let request = json!({
        "request_id": "owner-local-unavailable-hold",
        "caller_version": probe["binary_version"],
        "caller_schema": orbit_store::contracts::DISTRIBUTED_DRAIN_PROTOCOL_SCHEMA,
        "caller_fingerprint": fingerprint,
        "caller_before_pr": false,
        "review_gate": true,
        "ship": probe["ship"],
        "run_context": {"run_id": "owner-local-drain", "job_name": "workspace_pull_pipeline"},
    });
    let pulled = pair
        .wire
        .call("", "orbit.task.pull", request.clone())
        .expect("the failing machine pulls as itself");
    assert_eq!(pulled["receipt"]["machine_id"], OWNER, "{pulled:#}");
    assert!(pulled["receipt"]["claim"].is_null(), "{pulled:#}");
    let deferred = pulled["receipt"]["deferred_conflicts"]
        .as_array()
        .and_then(|held| held.iter().find(|held| held["task_id"] == task.as_str()))
        .unwrap_or_else(|| panic!("the failing machine still holds the task: {pulled:#}"));
    assert!(
        deferred["reason"].as_str().is_some_and(|reason| {
            reason.contains("provider hold") && reason.contains(&run) && reason.contains("sol")
        }),
        "{deferred}"
    );
    let replay = pair
        .wire
        .call("", "orbit.task.pull", request)
        .expect("replaying the same request");
    assert_eq!(replay, pulled, "the hold stays on the stored receipt");
    assert!(
        pair.owner_claims().is_empty(),
        "the failing machine took no claim: {:#?}",
        pair.owner_claims()
    );
    assert_eq!(pair.owner_status(&task), "backlog");

    *pair.wire.local.lock().unwrap() = false;
    let follower_drain = pair.run_drain();
    let follower_pass = pair.pass(&follower_drain);
    assert!(
        launch_refused(&follower_pass),
        "the other machine is admitted far enough to launch: {follower_pass}"
    );
    assert!(
        pair.owner_claims()
            .iter()
            .any(|claim| claim["claim"]["task_id"] == task.as_str()),
        "a different machine is not excluded by the hold: {:#?}",
        pair.owner_claims()
    );
}

/// An owner-local pipeline fails unavailable, the way a drain on this host
/// does, so finalization records the hold against the owner's machine.
fn fail_owner_unavailable(pair: &Pair, task: &str) -> String {
    let owner = &pair.wire.owner;
    let jobs = orbit_store::compose::workspace_job_run_store(
        owner.sqlite_store().unwrap(),
        owner.workspace_id().unwrap(),
    );
    let input = json!({ "task_ids": [task], "crew": "sol" });
    let run = jobs
        .insert_job_run(
            "task_local_pipeline",
            1,
            Utc::now(),
            Some(input.clone()),
            None,
        )
        .unwrap();
    jobs.mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
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
    let now = Utc::now();
    let message = format!(
        "step `implement_one`: {}",
        provider_failure_text(
            ProviderFailureClass::Unavailable,
            "codex",
            "codex provider authentication failure (HTTP 401): token expired",
        )
    );
    owner
        .complete_job_run_step(
            &run.run_id,
            &JobRunStepParams {
                lock_wait_ms: None,
                step_index: 1,
                target_type: JobTargetType::Activity,
                target_id: "implement_one".to_string(),
                started_at: now,
                finished_at: now,
                duration_ms: Some(1),
                exit_code: Some(1),
                agent_response_json: None,
                state: JobRunState::Failed,
                error_code: Some("STEP_FAILED".to_string()),
                error_message: Some(message),
            },
        )
        .unwrap();
    owner
        .finalize_job_run(&run.run_id, JobRunState::Failed, now, Some(1))
        .unwrap();
    run.run_id
}
