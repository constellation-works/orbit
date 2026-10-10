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
#[test]
fn a_state_pilot_whose_apply_requests_a_repair_settles_with_the_repair_apply() {
    if !super::super::dispatch_admission::isolated(
        "task_pilot::settlement::a_state_pilot_whose_apply_requests_a_repair_settles_with_the_repair_apply",
    ) {
        return;
    }
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
#[test]
fn a_step_inserted_before_apply_does_not_move_what_settles_a_member() {
    if !super::super::dispatch_admission::isolated(
        "task_pilot::settlement::a_step_inserted_before_apply_does_not_move_what_settles_a_member",
    ) {
        return;
    }
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
