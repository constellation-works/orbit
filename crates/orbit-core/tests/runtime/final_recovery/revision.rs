//! [ORB-15292] What counts as the task changing after the failure. Writes the
//! recovery itself may cause leave its decision standing; a lifecycle change
//! by an operator or orchestrator still refuses it.

use chrono::Utc;
use orbit_core::application::task::TaskUpdateParams;
use orbit_engine::{
    FinalRecoveryAdmission, FinalRecoveryApplied, RuntimeHost, TaskAutomationUpdate,
};
use orbit_types::task::{ContextWideningStep, TaskComment};
use orbit_types::workflow::FinalRecoveryDecision;
use serde_json::json;

use super::{Fixture, fixture};

fn requeue() -> FinalRecoveryDecision {
    FinalRecoveryDecision::Requeue {
        reason: "the missing tool now resolves".to_string(),
    }
}

/// A run of a fresh in-progress task whose failure admitted final recovery.
fn admitted(fixture: &Fixture) -> (String, String) {
    let task = fixture.task();
    let run = fixture.running_run(&task);
    assert_eq!(fixture.admit(&run, &task), FinalRecoveryAdmission::Admitted);
    (task, run)
}

/// One lifecycle change applied to a task after its failure.
type Change<'a> = dyn Fn(&str) + 'a;

fn updated_at(fixture: &Fixture, task: &str) -> chrono::DateTime<Utc> {
    fixture.runtime.get_task(task).unwrap().updated_at
}

#[test]
fn writes_the_recovery_may_cause_leave_its_requeue_standing() {
    if !super::super::dispatch_admission::isolated(
        "final_recovery::revision::writes_the_recovery_may_cause_leave_its_requeue_standing",
    ) {
        return;
    }
    let fixture = fixture("[\"sol\"]");
    let (task, run) = admitted(&fixture);
    let observed = updated_at(&fixture, &task);

    // The agent records the root cause, repairs a path outside the task's
    // selectors, and the run records its own comment and summary.
    fixture
        .runtime
        .run_tool(
            "orbit.friction.add",
            json!({"body": "The sandbox helper was missing.", "during_task": task, "model": "codex"}),
        )
        .unwrap();
    let widened = RuntimeHost::widen_task_context_files(
        &fixture.runtime,
        &task,
        &run,
        ContextWideningStep::Recovery,
        "final_recovery",
        &["src/repair.rs".to_string()],
    )
    .unwrap();
    assert_eq!(widened, ["file:src/repair.rs"]);
    RuntimeHost::apply_task_automation_update(
        &fixture.runtime,
        &task,
        TaskAutomationUpdate {
            execution_summary: Some("Recovery verified the environment.".to_string()),
            append_comments: vec![TaskComment {
                at: Utc::now(),
                by: "system".to_string(),
                message: format!("run {run}: final recovery repaired the worktree"),
            }],
            ..Default::default()
        },
    )
    .unwrap();
    assert!(
        updated_at(&fixture, &task) > observed,
        "the fixture's writes must advance the task's update time"
    );

    let applied = fixture.apply(&run, &task, requeue(), None);

    assert!(
        matches!(applied, FinalRecoveryApplied::Settled { .. }),
        "{applied:?}"
    );
    assert_eq!(fixture.status(&task), "backlog");
}

#[test]
fn a_lifecycle_change_after_the_failure_still_refuses_the_decision() {
    if !super::super::dispatch_admission::isolated(
        "final_recovery::revision::a_lifecycle_change_after_the_failure_still_refuses_the_decision",
    ) {
        return;
    }
    let fixture = fixture("[\"sol\"]");
    let human = |task: &str, params: TaskUpdateParams| {
        fixture
            .runtime
            .update_task_as_human(task, params, "human:operator".to_string())
            .unwrap();
    };
    let changes: [(&str, &Change); 6] = [
        ("an operator re-crew", &|task| {
            human(
                task,
                TaskUpdateParams {
                    crew: Some(Some("luna".to_string())),
                    ..Default::default()
                },
            )
        }),
        ("a context_files edit", &|task| {
            human(
                task,
                TaskUpdateParams {
                    context_files: Some(vec!["file:src/operator.rs".to_string()]),
                    ..Default::default()
                },
            )
        }),
        ("a description edit", &|task| {
            human(
                task,
                TaskUpdateParams {
                    description: Some("Narrowed by the operator.".to_string()),
                    ..Default::default()
                },
            )
        }),
        ("a dependency", &|task| {
            let other = fixture.task();
            human(
                task,
                TaskUpdateParams {
                    dependencies: Some(vec![other]),
                    ..Default::default()
                },
            )
        }),
        ("a status round trip", &|task| {
            for status in ["backlog", "in_progress"] {
                fixture
                    .runtime
                    .run_tool(
                        "orbit.task.update",
                        json!({"id": task, "status": status, "model": "codex"}),
                    )
                    .unwrap();
            }
        }),
        ("a human comment", &|task| {
            human(
                task,
                TaskUpdateParams {
                    comment: Some("Withdrawn: do not retry this.".to_string()),
                    ..Default::default()
                },
            )
        }),
    ];

    for (case, change) in changes {
        let (task, run) = admitted(&fixture);
        change(&task);

        let applied = fixture.apply(&run, &task, requeue(), None);

        let FinalRecoveryApplied::Escalated { outcome } = applied else {
            panic!("{case}: the later change must stand, got {applied:?}");
        };
        assert!(
            outcome.contains("task changed after the failure")
                && outcome.contains("the later decision stands"),
            "{case}: {outcome}"
        );
        assert_eq!(fixture.status(&task), "in-progress", "{case}");
    }
}
