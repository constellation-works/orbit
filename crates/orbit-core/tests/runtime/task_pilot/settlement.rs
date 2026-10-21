//! A state routine settles its members from the run's deterministic apply
//! steps, found by step id in the definition the run executed: a step added
//! before them, or another step's checkpoint at their old position, never
//! stands in for apply output [ORB-15197].

use super::races::apply_input;
use super::source_moves::{apply_claim, claimed, prepare_claim};
use super::*;

/// Mark `attempt`'s run running, as its worker does before any step.
fn start_run(workspace: &Workspace, attempt: &MemberAttempt) -> String {
    let run_id = attempt.action_id.clone().unwrap();
    workspace
        .jobs
        .mark_job_run_running(&run_id, Utc::now(), std::process::id())
        .unwrap();
    run_id
}

fn finish_run(workspace: &Workspace, run_id: &str) {
    workspace
        .jobs
        .finalize_job_run(run_id, JobRunState::Success, Utc::now(), None)
        .unwrap();
}

/// The certified receipt for `attempt` names exactly `task`.
fn assert_assessed(workspace: &Workspace, attempt: &MemberAttempt, task: &Task) {
    let members = workspace.routine_state().members.unwrap();
    assert!(members.active.is_none(), "{members:?}");
    assert!(members.failed.is_empty(), "{:?}", members.failed);
    assert!(!members.withheld.contains_key(&task.id), "{members:?}");
    assert_eq!(members.assessed[&task.id].receipt_id, attempt.id);
    let receipt = workspace
        .runtime
        .automation_store()
        .unwrap()
        .automation_receipt(&attempt.consumer, &attempt.id)
        .unwrap()
        .expect("settlement records a receipt");
    assert_eq!(
        receipt.evidence_reference,
        format!(
            "run:{}/deterministic-apply/{}",
            attempt.action_id.as_deref().unwrap(),
            task.id
        )
    );
}

/// The first apply rejects the pilot's assessment and requests a targeted
/// repair. Until the repair apply records, the member stays claimed, so the
/// repair apply still finds its claim and the run settles assessed.
fn a_state_pilot_whose_apply_requests_a_repair_settles_with_the_repair_apply() {
    let workspace = Workspace::new();
    workspace.install_pilot_job();
    let task = workspace.task("needs a repair");
    let routine = pilot_routine();
    let now = Utc::now();
    evaluate_routine(&workspace.runtime, &routine, false, now).unwrap();
    let attempt = workspace.admitted(&task, 2);
    let run_id = start_run(&workspace, &attempt);

    let prepared = prepare_claim(&workspace, &attempt);
    let mut input = apply_input(&workspace, &prepared);
    input["results"][0]["tasks"][0]["recommended_complexity"] = json!("invalid");
    let pilots = input["results"].clone();
    // The pilot fan-in checkpoints before apply runs; its output is no
    // apply record, so the running run settles nothing yet.
    workspace.record_steps(&run_id, &[("prepare", &prepared), ("pilots", &pilots)]);
    let running = evaluate_routine(
        &workspace.runtime,
        &routine,
        false,
        now + Duration::minutes(3),
    )
    .unwrap();
    assert_eq!(running.reason, "batch_pending", "{running:?}");

    let first = claimed(&workspace, &attempt, "apply_task_pilot_results", input);
    assert_eq!(first["repair_count"], 1, "{first}");
    workspace.record_steps(&run_id, &[("apply", &first)]);
    let repairing = evaluate_routine(
        &workspace.runtime,
        &routine,
        false,
        now + Duration::minutes(4),
    )
    .unwrap();
    assert_eq!(repairing.reason, "batch_pending", "{repairing:?}");
    let members = workspace.routine_state().members.unwrap();
    assert_eq!(
        members.active.as_ref().map(|active| &active.id),
        Some(&attempt.id),
        "the claim outlives an apply that requested a repair"
    );
    assert!(members.failed.is_empty(), "{:?}", members.failed);

    // The repair apply passes the claim check the first apply settled for.
    let mut repair = apply_input(&workspace, &first["repair_prepared"]);
    repair["prior_applied_count"] = first["applied_count"].clone();
    repair["carried_task_outcomes"] = first["non_repairable_outcomes"].clone();
    let repaired = claimed(&workspace, &attempt, "apply_task_pilot_results", repair);
    assert_eq!(repaired["status"], "succeeded", "{repaired}");
    assert_eq!(repaired["applied_count"], 1, "{repaired}");
    assert_eq!(
        repaired["member_evidence"][0]["member_key"], task.id,
        "{repaired}"
    );
    assert_eq!(
        workspace.action("pipeline_success_guard", json!({"result": repaired}))["succeeded"],
        true
    );
    workspace.record_steps(
        &run_id,
        &[
            ("repair_pilots", &first["repair_partitions"]),
            ("apply_repairs", &repaired),
        ],
    );
    finish_run(&workspace, &run_id);

    evaluate_routine(
        &workspace.runtime,
        &routine,
        false,
        now + Duration::minutes(5),
    )
    .unwrap();
    assert_assessed(&workspace, &attempt, &task);
}

/// The shipped definition with a step inserted before `apply`: settlement
/// still reads apply by its id, and the checkpoint now at apply's former
/// position settles nothing.
fn a_step_inserted_before_apply_does_not_move_what_settles_a_member() {
    let workspace = Workspace::new();
    workspace.install_pilot_job();
    let path = workspace
        .runtime
        .global_root()
        .join("resources/jobs/task_pilot_pipeline.yaml");
    let shipped_apply = workspace.pilot_step_index("apply");
    let mut job: serde_yaml::Value =
        serde_yaml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let steps = job["spec"]["steps"].as_sequence_mut().unwrap();
    let apply = steps
        .iter()
        .position(|step| step["id"].as_str() == Some("apply"))
        .unwrap();
    steps.insert(
        apply,
        serde_yaml::from_str(
            "id: inserted_before_apply\n\
             when: \"{{ steps.prepare.output.partition_count }} == 0\"\n\
             target: activity:pipeline_success_guard\n\
             default_input:\n  context: inserted fixture step\n  result: {}\n",
        )
        .unwrap(),
    );
    std::fs::write(&path, serde_yaml::to_string(&job).unwrap()).unwrap();
    assert_eq!(workspace.pilot_step_index("apply"), shipped_apply + 1);

    let task = workspace.task("applies after an inserted step");
    let routine = pilot_routine();
    let now = Utc::now();
    evaluate_routine(&workspace.runtime, &routine, false, now).unwrap();
    let attempt = workspace.admitted(&task, 2);
    let run_id = start_run(&workspace, &attempt);
    let prepared = prepare_claim(&workspace, &attempt);
    let applied = apply_claim(&workspace, &attempt, &prepared);
    assert_eq!(applied["status"], "succeeded", "{applied}");
    assert_eq!(applied["repair_count"], 0, "{applied}");
    workspace.record_steps(
        &run_id,
        &[
            ("prepare", &prepared),
            ("pilots", &json!([])),
            ("inserted_before_apply", &Value::Null),
        ],
    );
    let state = workspace.runtime.read_run_state(&run_id).unwrap().unwrap();
    assert_eq!(
        state.step_states.get(&shipped_apply),
        Some(&JobRunState::Success),
        "apply's shipped position holds another step's checkpoint"
    );
    let running = evaluate_routine(
        &workspace.runtime,
        &routine,
        false,
        now + Duration::minutes(3),
    )
    .unwrap();
    assert_eq!(running.reason, "batch_pending", "{running:?}");

    workspace.record_steps(&run_id, &[("apply", &applied)]);
    finish_run(&workspace, &run_id);
    evaluate_routine(
        &workspace.runtime,
        &routine,
        false,
        now + Duration::minutes(4),
    )
    .unwrap();
    assert_assessed(&workspace, &attempt, &task);
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    a_state_pilot_whose_apply_requests_a_repair_settles_with_the_repair_apply_scenarios,
    a_state_pilot_whose_apply_requests_a_repair_settles_with_the_repair_apply,
    a_step_inserted_before_apply_does_not_move_what_settles_a_member
);

/// Delivery routines must settle a stopped job without coverage, including
/// Held and Timeout. Neither terminal state automatically retries: a hold
/// needs operator attention, and a timeout requires explicit recovery.
#[cfg(unix)]
fn a_delivery_routine_settles_held_and_timed_out_jobs_without_retrying() {
    use orbit_automation::delivery::input_digest;
    use orbit_types::workflow::automation::{
        BatchAttempt, BatchState, CoverageBatch, Delivery, SourceRevision,
    };

    for terminal in [JobRunState::Held, JobRunState::Timeout] {
        let workspace = Workspace::new();
        let routine: RoutineDefinition = serde_json::from_value(json!({
            "schemaVersion": 1, "name": "fixture-delivery", "enabled": true,
            "target": "job:task_pilot_pipeline",
            "policy": {"retries": {"max": 1}},
            "trigger": {"deliveries_landed": {
                "owner_machine": "fixture-machine", "branch": "main",
                "threshold": 1, "max_wait_minutes": 10,
                "coverage": "landed_code_review_v1", "retries": 1,
            }},
        }))
        .unwrap();
        let now = Utc::now();
        let baseline = evaluate_routine(&workspace.runtime, &routine, false, now).unwrap();
        assert_eq!(baseline.reason, "baselined", "{baseline:?}");
        let state = baseline.state.unwrap();
        workspace.commit_file("README.md", "landed change\n", "landed change");
        let after = SourceRevision {
            commit: workspace.object("HEAD"),
            tree: workspace.object("HEAD^{tree}"),
        };
        let delivery = Delivery {
            key: "fixture-landing".into(),
            repository: state.repository.clone(),
            branch: state.branch.clone(),
            before: state.baseline.clone(),
            after: after.clone(),
            commits: vec![after.commit.clone()],
            task_ids: vec![],
            unattributed: Some("no_landing_task".into()),
            evidence_reference: "fixture-landing".into(),
            evidence_digest: digest(b"fixture-landing"),
            landed_at: now,
        };
        // Freeze and admit through the real stores, without starting a worker
        // or a provider. Two attempts leave budget to expose accidental retries.
        let batch = CoverageBatch {
            schema_version: 1,
            id: digest(b"delivery settlement"),
            consumer: state.consumer.clone(),
            epoch: state.epoch.clone(),
            repository: state.repository.clone(),
            branch: state.branch.clone(),
            coverage: routine.trigger.deliveries_landed.as_ref().unwrap().coverage,
            from_exclusive: state.baseline.clone(),
            through_inclusive: after.clone(),
            commits: vec![after.commit.clone()],
            deliveries: vec![delivery.clone()],
            exclusions: vec![],
            created_at: now,
            max_attempts: 2,
            retry_until: now + Duration::hours(1),
        };
        let mut attempt = BatchAttempt {
            input_digest: input_digest(&batch).unwrap(),
            action_key: format!("automation:{}:1", batch.id),
            batch,
            attempt: 1,
            action_id: None,
            state: BatchState::Admitted,
            reason: None,
            retry_after: None,
            reissue: None,
        };
        let KeyedJobRunAdmission::Admitted(run) = workspace
            .jobs
            .insert_automation_job_run(
                "task_pilot_pipeline",
                json!({"automation": attempt}),
                &attempt.action_key,
            )
            .unwrap()
        else {
            panic!("fresh delivery action must be admitted")
        };
        attempt.action_id = Some(run.run_id.clone());
        let mut admitted = state.clone();
        admitted.generation += 1;
        admitted.observed = after.clone();
        admitted.pending_commits = vec![after.commit.clone()];
        admitted.pending = vec![delivery];
        admitted.active = Some(attempt.clone());
        let store = workspace.runtime.automation_store().unwrap();
        assert!(store.automation_commit(&state, &admitted, None).unwrap());
        // A completed child supplies a conclusively stopped owner without
        // relying on a guessed PID, timing, or load on this shared host.
        let mut owner = crate::git_test_support::command()
            .args(["hash-object", "--stdin"])
            .current_dir(&workspace.repo)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        workspace
            .jobs
            .mark_job_run_running(&run.run_id, now, owner.id())
            .unwrap();
        // EOF releases the child after its identity has been recorded.
        drop(owner.stdin.take());
        assert!(owner.wait().unwrap().success());
        workspace
            .jobs
            .finalize_job_run(&run.run_id, terminal, now, Some(1))
            .unwrap();
        let stopped = workspace.runtime.show_job_run(&run.run_id).unwrap();
        assert_eq!(stopped.state, terminal);
        assert!(
            stopped
                .steps
                .iter()
                .all(|step| step.agent_response_json.is_none())
        );

        for minutes in [1, 2] {
            let result = evaluate_routine(
                &workspace.runtime,
                &routine,
                false,
                now + Duration::minutes(minutes),
            )
            .unwrap();
            assert_eq!(result.reason, "needs_attention", "{terminal:?}: {result:?}");
            let persisted = store.automation_state(&state.consumer).unwrap().unwrap();
            let active = persisted.active.as_ref().unwrap();
            assert_eq!(active.state, BatchState::Failed, "{terminal:?}: {active:?}");
            assert_eq!(
                active.reason.as_deref(),
                Some("job_stopped_without_accepted_evidence")
            );
            assert_eq!(
                active.attempt, 1,
                "{terminal:?} must not automatically retry"
            );
            assert_eq!(active.action_id, attempt.action_id);
            assert_eq!(
                active.batch, attempt.batch,
                "the original obligations remain owed"
            );
            assert_eq!(persisted.covered, state.covered);
            assert_eq!(persisted.pending_commits, admitted.pending_commits);
            assert!(
                result.receipts.is_empty(),
                "no evidence means no coverage receipt"
            );
        }
    }
}

#[cfg(unix)]
crate::scenario_table!(
    a_delivery_routine_settles_held_and_timed_out_jobs_without_retrying_scenarios,
    a_delivery_routine_settles_held_and_timed_out_jobs_without_retrying
);
