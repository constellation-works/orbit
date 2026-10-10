//! [ORB-15292] What counts as the task changing after the failure. Writes the
//! recovery itself may cause leave its decision standing; a lifecycle change
//! by an operator or orchestrator still refuses it, including an edit to
//! selectors an earlier recovery widened [ORB-15305].

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

fn context_files(fixture: &Fixture, task: &str) -> Vec<String> {
    fixture.runtime.get_task(task).unwrap().context_files
}

/// Widen `task` over `path` as a recovery agent's repair in `run` does.
fn recovery_widens(fixture: &Fixture, task: &str, run: &str, path: &str) {
    let widened = RuntimeHost::widen_task_context_files(
        &fixture.runtime,
        task,
        run,
        ContextWideningStep::Recovery,
        "final_recovery",
        &[path.to_string()],
    )
    .unwrap();
    assert_eq!(widened, [format!("file:{path}")]);
}

fn set_scope(fixture: &Fixture, task: &str, selectors: Vec<String>) {
    fixture
        .runtime
        .update_task_as_human(
            task,
            TaskUpdateParams {
                context_files: Some(selectors),
                ..Default::default()
            },
            "human:operator".to_string(),
        )
        .unwrap();
}

/// A fresh in-progress task an earlier run's recovery widened over
/// `src/recovered.rs`.
fn recovered_earlier(fixture: &Fixture) -> String {
    let task = fixture.task();
    let earlier = fixture.running_run(&task);
    recovery_widens(fixture, &task, &earlier, "src/recovered.rs");
    task
}

/// A run of `task` whose failure admitted final recovery.
fn admit_run(fixture: &Fixture, task: &str) -> String {
    let run = fixture.running_run(task);
    assert_eq!(fixture.admit(&run, task), FinalRecoveryAdmission::Admitted);
    run
}

fn assert_refused(case: &str, applied: FinalRecoveryApplied) {
    let FinalRecoveryApplied::Escalated { outcome } = applied else {
        panic!("{case}: the later change must stand, got {applied:?}");
    };
    assert!(
        outcome.contains("task changed after the failure")
            && outcome.contains("the later decision stands"),
        "{case}: {outcome}"
    );
}

#[test]
fn writes_the_recovery_may_cause_leave_its_requeue_standing() {
    if !super::super::dispatch_admission::isolated(
        "final_recovery::revision::writes_the_recovery_may_cause_leave_its_requeue_standing",
    ) {
        return;
    }
    let fixture = fixture("[\"sol\"]");
    // An earlier run's recovery widened the task too; its selector is
    // ordinary scope now and does not disturb this recovery's exemption.
    let task = recovered_earlier(&fixture);
    let run = admit_run(&fixture, &task);
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
    recovery_widens(&fixture, &task, &run, "src/repair.rs");
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

        assert_refused(case, applied);
        assert_eq!(fixture.status(&task), "in-progress", "{case}");
    }
}

#[test]
fn an_edit_to_recovery_widened_selectors_after_the_failure_refuses_the_decision() {
    if !super::super::dispatch_admission::isolated(
        "final_recovery::revision::an_edit_to_recovery_widened_selectors_after_the_failure_refuses_the_decision",
    ) {
        return;
    }
    let fixture = fixture("[\"sol\"]");
    let recovered = "file:src/recovered.rs".to_string();
    let without = |selectors: Vec<String>, selector: &str| {
        selectors
            .into_iter()
            .filter(|existing| existing != selector)
            .collect::<Vec<_>>()
    };

    // An operator drops a selector an earlier recovery widened.
    let task = recovered_earlier(&fixture);
    let run = admit_run(&fixture, &task);
    let scope = without(context_files(&fixture, &task), &recovered);
    set_scope(&fixture, &task, scope.clone());
    let applied = fixture.apply(&run, &task, requeue(), None);
    assert_refused("removing an earlier recovery's selector", applied);
    assert_eq!(fixture.status(&task), "in-progress");
    assert_eq!(context_files(&fixture, &task), scope);

    // An operator drops it before the failure and restores it after.
    let task = recovered_earlier(&fixture);
    let narrowed = without(context_files(&fixture, &task), &recovered);
    set_scope(&fixture, &task, narrowed.clone());
    let run = admit_run(&fixture, &task);
    let restored = [narrowed, vec![recovered.clone()]].concat();
    set_scope(&fixture, &task, restored.clone());
    let applied = fixture.apply(&run, &task, requeue(), None);
    assert_refused("reintroducing an earlier recovery's selector", applied);
    assert_eq!(fixture.status(&task), "in-progress");
    assert_eq!(context_files(&fixture, &task), restored);

    // An operator drops the selector this recovery just widened.
    let (task, run) = admitted(&fixture);
    let before = context_files(&fixture, &task);
    recovery_widens(&fixture, &task, &run, "src/repair.rs");
    set_scope(&fixture, &task, before.clone());
    let applied = fixture.apply(&run, &task, requeue(), None);
    assert_refused("removing this recovery's selector", applied);
    assert_eq!(fixture.status(&task), "in-progress");
    assert_eq!(context_files(&fixture, &task), before);
}

#[test]
fn a_revision_observed_before_the_digest_compares_by_update_time() {
    if !super::super::dispatch_admission::isolated(
        "final_recovery::revision::a_revision_observed_before_the_digest_compares_by_update_time",
    ) {
        return;
    }
    let fixture = fixture("[\"sol\"]");
    let legacy = |run: &str| {
        let mut state = fixture.state(run);
        let checkpoint = state.final_recovery.as_mut().unwrap();
        checkpoint.observed.as_mut().unwrap().lifecycle_digest = None;
        fixture.jobs.write_run_state(run, &state).unwrap();
    };

    let (task, run) = admitted(&fixture);
    legacy(&run);
    let applied = fixture.apply(&run, &task, requeue(), None);
    assert!(
        matches!(applied, FinalRecoveryApplied::Settled { .. }),
        "an untouched task settles: {applied:?}"
    );
    assert_eq!(fixture.status(&task), "backlog");

    // Run bookkeeping a digest would ignore still advances the update time.
    let (task, run) = admitted(&fixture);
    legacy(&run);
    let observed = updated_at(&fixture, &task);
    RuntimeHost::apply_task_automation_update(
        &fixture.runtime,
        &task,
        TaskAutomationUpdate {
            execution_summary: Some("Recovery verified the environment.".to_string()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(updated_at(&fixture, &task) > observed);
    let applied = fixture.apply(&run, &task, requeue(), None);
    assert_refused("any write after a legacy observation", applied);
    assert_eq!(fixture.status(&task), "in-progress");
}
