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

/// A one-task partition whose assessment the apply rejects — empty
/// `context_files` without a `verified_no_diff`/`host_operational`
/// disposition — in both the first apply and the targeted repair apply:
/// the guard records the run held (not failed) and the unresolved task
/// stays held for the operator instead of being assessed or re-pending
/// [ORB-24083].
fn a_one_task_apply_rejected_after_repair_is_held_not_failed() {
    let workspace = Workspace::new();
    workspace.install_pilot_job();
    let task = workspace.task("rejected after repair");
    let routine = pilot_routine();
    let now = Utc::now();
    evaluate_routine(&workspace.runtime, &routine, false, now).unwrap();
    let attempt = workspace.admitted(&task, 2);
    let run_id = start_run(&workspace, &attempt);

    let prepared = prepare_claim(&workspace, &attempt);
    let mut input = apply_input(&workspace, &prepared);
    input["results"][0]["tasks"][0]["context_files_after"] = json!([]);
    let first = claimed(&workspace, &attempt, "apply_task_pilot_results", input);
    assert_eq!(first["status"], "failed", "{first}");
    assert_eq!(first["applied_count"], 0, "{first}");
    assert_eq!(first["unresolved_count"], 1, "{first}");
    assert_eq!(first["repair_count"], 1, "{first}");
    assert_eq!(first["task_outcomes"][0]["outcome"], "invalid", "{first}");
    assert!(
        first["task_outcomes"][0]["error"]
            .as_str()
            .is_some_and(|error| error.contains("may keep empty context_files")),
        "{first}"
    );
    workspace.record_steps(&run_id, &[("prepare", &prepared), ("apply", &first)]);

    let mut repair = apply_input(&workspace, &first["repair_prepared"]);
    repair["results"][0]["tasks"][0]["context_files_after"] = json!([]);
    repair["prior_applied_count"] = first["applied_count"].clone();
    repair["carried_task_outcomes"] = first["non_repairable_outcomes"].clone();
    let repaired = claimed(&workspace, &attempt, "apply_task_pilot_results", repair);
    assert_eq!(repaired["status"], "failed", "{repaired}");
    assert_eq!(repaired["applied_count"], 0, "{repaired}");
    assert_eq!(repaired["unresolved_count"], 1, "{repaired}");
    assert_eq!(
        repaired["task_outcomes"][0]["outcome"], "invalid",
        "{repaired}"
    );

    let guard = workspace.action(
        "pipeline_success_guard",
        json!({"context": "task-pilot targeted repair apply", "result": repaired}),
    );
    assert_eq!(guard["succeeded"], true, "{guard}");
    assert_eq!(guard["held_count"], 1, "{guard}");
    assert_eq!(guard["rejected_apply_held_count"], 1, "{guard}");

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
    let members = workspace.routine_state().members.unwrap();
    assert!(members.active.is_none(), "{members:?}");
    assert!(!members.assessed.contains_key(&task.id), "{members:?}");
    assert!(members.failed.contains_key(&task.id), "{members:?}");
    assert!(
        members.failed[&task.id].exhausted,
        "the failed attempt is exhausted, so the task is held for the operator: {members:?}"
    );
    assert!(
        workspace
            .runtime
            .get_task_history(&task.id)
            .unwrap()
            .iter()
            .all(|entry| entry.event != "task_pilot_applied"),
        "a rejected assessment writes no pilot receipt"
    );
}

/// The guard still fails an apply that failed for another reason, and
/// still passes the partial apply (applied > 0, unresolved > 0) as held
/// [ORB-24083, ORB-21172].
fn other_apply_failures_still_fail_and_partial_apply_still_holds() {
    let workspace = Workspace::new();
    let guard = |input: Value| {
        workspace
            .runtime
            .run_deterministic(
                "pipeline_success_guard",
                &json!({}),
                &input,
                ToolContext::default(),
            )
            .map_err(|error| error.to_string())
    };
    let rejected = json!({
        "status": "failed", "applied_count": 0, "unresolved_count": 1,
        "error": "task-pilot apply unresolved: partition 0, task T: invalid: x; 0 applied, 1 unresolved",
        "partition_decisions": [{
            "partition_index": 0, "outcome": "failed", "applied_task_ids": [],
            "task_outcomes": [{"task_id": "T", "outcome": "invalid", "error": "x"}],
            "unresolved_count": 1,
        }],
        "task_outcomes": [{"task_id": "T", "outcome": "invalid", "error": "x"}],
    });
    // A failed partition whose task did not settle as `invalid`.
    let mut failed_partition = rejected.clone();
    failed_partition["partition_decisions"][0]["task_outcomes"][0]["outcome"] =
        json!("apply_failed");
    failed_partition["task_outcomes"][0]["outcome"] = json!("apply_failed");
    assert!(
        guard(json!({"result": failed_partition})).is_err(),
        "a failed partition that is not an apply rejection still fails"
    );
    // A skipped (stale) partition.
    let mut skipped = rejected.clone();
    skipped["partition_decisions"][0]["outcome"] = json!("skipped_stale");
    skipped["partition_decisions"][0]["task_outcomes"][0]["outcome"] = json!("stale");
    skipped["task_outcomes"][0]["outcome"] = json!("stale");
    assert!(
        guard(json!({"result": skipped})).is_err(),
        "a skipped partition still fails"
    );
    // A missing apply envelope.
    let missing = json!({"status": "failed", "applied_count": 0, "unresolved_count": 1});
    assert!(
        guard(json!({"result": missing})).is_err(),
        "a missing apply envelope still fails"
    );
    // No unresolved tasks.
    let mut none_unresolved = rejected.clone();
    none_unresolved["unresolved_count"] = json!(0);
    assert!(
        guard(json!({"result": none_unresolved})).is_err(),
        "an apply with no unresolved tasks still fails"
    );
    // The existing partial apply still passes as held.
    let mut partial = rejected.clone();
    partial["applied_count"] = json!(1);
    partial["unresolved_count"] = json!(1);
    partial["partition_decisions"][0]["outcome"] = json!("partial");
    partial["partition_decisions"][0]["applied_task_ids"] = json!(["U"]);
    partial["task_outcomes"] = json!([
        {"task_id": "U", "outcome": "applied", "error": null},
        {"task_id": "T", "outcome": "invalid", "error": "x"},
    ]);
    let held = guard(json!({"result": partial})).expect("a partial apply is held");
    assert_eq!(held["succeeded"], true, "{held}");
    assert_eq!(held["held_count"], 1, "{held}");
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    a_state_pilot_whose_apply_requests_a_repair_settles_with_the_repair_apply_scenarios,
    a_state_pilot_whose_apply_requests_a_repair_settles_with_the_repair_apply,
    a_step_inserted_before_apply_does_not_move_what_settles_a_member,
    a_one_task_apply_rejected_after_repair_is_held_not_failed,
    other_apply_failures_still_fail_and_partial_apply_still_holds
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
