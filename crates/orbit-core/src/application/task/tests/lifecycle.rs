//! The lifecycle table every attributed status change is checked against
//! [ORB-12245].

use chrono::Utc;
use orbit_common::OrbitError;
use orbit_types::task::{Task, TaskStatus};
use orbit_types::workflow::JobRunState;

use super::super::lifecycle::task_status_transition_allowed;
use super::test_runtime;
use crate::OrbitRuntime;
use crate::application::task::{TaskAddParams, TaskUpdateParams};

const ALL_STATUSES: [TaskStatus; 9] = [
    TaskStatus::Proposed,
    TaskStatus::Backlog,
    TaskStatus::Someday,
    TaskStatus::InProgress,
    TaskStatus::Review,
    TaskStatus::Done,
    TaskStatus::Blocked,
    TaskStatus::Archived,
    TaskStatus::Rejected,
];

/// A task that already carries both completion preconditions, so a matrix
/// walk measures the transition table alone.
fn add_task_with_evidence(runtime: &OrbitRuntime, title: &str) -> Task {
    runtime
        .add_task(TaskAddParams {
            title: title.to_string(),
            description: "Exercise the lifecycle table.".to_string(),
            acceptance_criteria: vec!["Only legal transitions are written.".to_string()],
            plan: "1) do the thing 2) verify".to_string(),
            ..Default::default()
        })
        .expect("add task")
}

/// Put a fixture task into `status` without consulting the table: seeding is
/// an in-crate concern, and the guarded surface is what the tests measure.
fn seed_status(runtime: &OrbitRuntime, id: &str, status: TaskStatus) {
    runtime
        .update_task(
            id,
            TaskUpdateParams {
                status: Some(status),
                execution_summary: Some("did the thing; verified".to_string()),
                ..Default::default()
            },
        )
        .expect("seed fixture status");
}

/// The guarded surface: exactly what `orbit.task.update`, the dashboard, and
/// the CLI `task update` reach.
fn guarded_update(
    runtime: &OrbitRuntime,
    id: &str,
    params: TaskUpdateParams,
) -> Result<Task, OrbitError> {
    runtime.update_task_with_identity(id, params, None, None)
}

fn guarded_status(
    runtime: &OrbitRuntime,
    id: &str,
    status: TaskStatus,
) -> Result<Task, OrbitError> {
    guarded_update(
        runtime,
        id,
        TaskUpdateParams {
            status: Some(status),
            ..Default::default()
        },
    )
}

#[test]
fn guarded_update_writes_every_legal_edge_and_refuses_every_other_one() {
    let (_root, runtime) = test_runtime();

    for from in ALL_STATUSES {
        for to in ALL_STATUSES {
            let task = add_task_with_evidence(&runtime, &format!("Matrix {from} to {to}"));
            seed_status(&runtime, &task.id, from);

            match guarded_status(&runtime, &task.id, to) {
                Ok(updated) => {
                    assert!(
                        task_status_transition_allowed(from, to),
                        "{from} -> {to} was written but the table refuses it"
                    );
                    assert_eq!(updated.status, to);
                }
                Err(error) => {
                    assert!(
                        !task_status_transition_allowed(from, to),
                        "{from} -> {to} is a legal edge but was refused: {error}"
                    );
                    assert!(
                        matches!(error, OrbitError::InvalidInput(_)),
                        "{from} -> {to} must be invalid_input, got {error}"
                    );
                    let message = error.to_string();
                    assert!(
                        message.contains(&format!("from '{from}' to '{to}'")),
                        "{from} -> {to} must name the pair: {message}"
                    );
                    assert_eq!(
                        runtime.get_task(&task.id).expect("reread task").status,
                        from,
                        "a refused transition must not be written"
                    );
                }
            }
        }
    }
}

#[test]
fn done_is_unreachable_from_proposed_and_backlog() {
    let (_root, runtime) = test_runtime();

    for from in [TaskStatus::Proposed, TaskStatus::Backlog] {
        let task = add_task_with_evidence(&runtime, &format!("Complete from {from}"));
        seed_status(&runtime, &task.id, from);

        let error = guarded_status(&runtime, &task.id, TaskStatus::Done)
            .expect_err("completion must not skip review");
        let message = error.to_string();
        assert!(
            message.contains("'done' is reachable only from 'review'"),
            "{from}: {message}"
        );
    }
}

#[test]
fn in_progress_requires_a_plan_exactly_like_task_start() {
    let (_root, runtime) = test_runtime();
    let task = runtime
        .add_task(TaskAddParams {
            title: "Unplanned work".to_string(),
            description: "Starting without a plan is refused on every surface.".to_string(),
            ..Default::default()
        })
        .expect("add unplanned task");

    let refused = guarded_status(&runtime, &task.id, TaskStatus::InProgress)
        .expect_err("an unplanned start is refused");
    let started = runtime
        .start_task(&task.id, None, None)
        .expect_err("task.start refuses the same task");
    assert_eq!(refused.to_string(), started.to_string());
    assert_eq!(
        runtime.get_task(&task.id).expect("reread task").status,
        TaskStatus::Proposed
    );

    // The plan may arrive on the write that transitions.
    let planned = guarded_update(
        &runtime,
        &task.id,
        TaskUpdateParams {
            plan: Some("1) reproduce 2) fix".to_string()),
            status: Some(TaskStatus::InProgress),
            ..Default::default()
        },
    )
    .expect("a planned start is accepted");
    assert_eq!(planned.status, TaskStatus::InProgress);
}

#[test]
fn completion_requires_a_summary_or_a_successful_run() {
    let (_root, runtime) = test_runtime();
    let task = add_task_with_evidence(&runtime, "Evidence for completion");
    runtime
        .update_task(
            &task.id,
            TaskUpdateParams {
                status: Some(TaskStatus::Review),
                ..Default::default()
            },
        )
        .expect("seed a reviewed task without evidence");

    let error = guarded_status(&runtime, &task.id, TaskStatus::Done)
        .expect_err("completion without evidence is refused");
    assert!(error.to_string().contains("execution summary"), "{error}");

    // A failed run is not evidence of completion.
    let failed_run = finished_run(&runtime, "failed-delivery", JobRunState::Failed);
    let error = guarded_update(
        &runtime,
        &task.id,
        TaskUpdateParams {
            job_run_id: Some(Some(failed_run)),
            status: Some(TaskStatus::Done),
            ..Default::default()
        },
    )
    .expect_err("a failed run does not complete a task");
    assert!(
        error.to_string().contains("finished successfully"),
        "{error}"
    );

    let succeeded_run = finished_run(&runtime, "successful-delivery", JobRunState::Success);
    let completed = guarded_update(
        &runtime,
        &task.id,
        TaskUpdateParams {
            job_run_id: Some(Some(succeeded_run)),
            status: Some(TaskStatus::Done),
            ..Default::default()
        },
    )
    .expect("a successful run completes the task");
    assert_eq!(completed.status, TaskStatus::Done);
}

#[test]
fn completion_accepts_a_summary_written_by_the_same_update() {
    let (_root, runtime) = test_runtime();
    let task = add_task_with_evidence(&runtime, "Summary with the transition");
    runtime
        .update_task(
            &task.id,
            TaskUpdateParams {
                status: Some(TaskStatus::Review),
                ..Default::default()
            },
        )
        .expect("seed a reviewed task without evidence");

    let completed = guarded_update(
        &runtime,
        &task.id,
        TaskUpdateParams {
            execution_summary: Some("delivered and verified".to_string()),
            status: Some(TaskStatus::Done),
            ..Default::default()
        },
    )
    .expect("the summary on this write satisfies completion");
    assert_eq!(completed.status, TaskStatus::Done);
}

#[test]
fn terminal_statuses_stay_closed_while_a_rejection_can_be_reconsidered() {
    let (_root, runtime) = test_runtime();

    for target in [
        TaskStatus::Proposed,
        TaskStatus::Backlog,
        TaskStatus::InProgress,
    ] {
        let task = add_task_with_evidence(&runtime, &format!("Reopen done to {target}"));
        seed_status(&runtime, &task.id, TaskStatus::Done);
        let error =
            guarded_status(&runtime, &task.id, target).expect_err("done work does not reopen");
        assert!(
            error.to_string().contains("regression_from"),
            "{target}: {error}"
        );
    }

    for target in [TaskStatus::Backlog, TaskStatus::InProgress] {
        let task = add_task_with_evidence(&runtime, &format!("Reconsider rejection as {target}"));
        seed_status(&runtime, &task.id, TaskStatus::Rejected);
        let reconsidered = guarded_status(&runtime, &task.id, target)
            .unwrap_or_else(|error| panic!("rejected -> {target} must stay open: {error}"));
        assert_eq!(reconsidered.status, target);
    }
}

#[test]
fn forcing_a_refused_transition_records_the_override_in_history() {
    let (_root, runtime) = test_runtime();
    let task = add_task_with_evidence(&runtime, "Forced reopen");
    seed_status(&runtime, &task.id, TaskStatus::Done);
    guarded_status(&runtime, &task.id, TaskStatus::Backlog).expect_err("guarded reopen is refused");

    let reopened = runtime
        .force_update_task_with_identity(
            &task.id,
            TaskUpdateParams {
                status: Some(TaskStatus::Backlog),
                ..Default::default()
            },
            None,
            None,
        )
        .expect("a human may override the table");
    assert_eq!(reopened.status, TaskStatus::Backlog);
    // The override preserves the delivered work's evidence.
    assert_eq!(reopened.execution_summary, "did the thing; verified");

    let history = runtime.get_task_history(&task.id).expect("task history");
    let forced = history.last().expect("forced event");
    assert_eq!(forced.event, "forced");
    assert_eq!(forced.from_status, Some(TaskStatus::Done));
    assert_eq!(forced.to_status, Some(TaskStatus::Backlog));
}

#[test]
fn forcing_a_legal_transition_still_records_it_as_an_override() {
    let (_root, runtime) = test_runtime();
    let task = add_task_with_evidence(&runtime, "Forced but legal");

    runtime
        .force_update_task_with_identity(
            &task.id,
            TaskUpdateParams {
                status: Some(TaskStatus::Backlog),
                ..Default::default()
            },
            None,
            None,
        )
        .expect("force applies a legal transition too");

    let history = runtime.get_task_history(&task.id).expect("task history");
    assert_eq!(history.last().expect("forced event").event, "forced");
}

/// `archive_task` and the delivery pipeline's activities are in-crate callers
/// that own their own transition rules, so the table does not second-guess
/// them — archiving delivered work stays a one-step operator action.
#[test]
fn internal_callers_keep_transitions_the_table_refuses() {
    let (_root, runtime) = test_runtime();
    let task = add_task_with_evidence(&runtime, "Archive delivered work");
    seed_status(&runtime, &task.id, TaskStatus::Done);

    runtime.archive_task(&task.id).expect("archive done task");
    assert_eq!(
        runtime.get_task(&task.id).expect("reread task").status,
        TaskStatus::Archived
    );
}

fn finished_run(runtime: &OrbitRuntime, job_id: &str, state: JobRunState) -> String {
    let run = runtime
        .stores()
        .jobs()
        .insert_job_run(job_id, 1, Utc::now(), None, None)
        .expect("insert run");
    runtime
        .stores()
        .jobs()
        .mark_job_run_running(&run.run_id, Utc::now(), std::process::id())
        .expect("start run");
    runtime
        .stores()
        .jobs()
        .finalize_job_run(&run.run_id, state, Utc::now(), Some(1))
        .expect("finalize run");
    run.run_id.to_string()
}
