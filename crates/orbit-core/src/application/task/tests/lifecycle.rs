//! The lifecycle table every attributed status change is checked against
//! [ORB-12245].

use orbit_common::OrbitError;
use orbit_types::task::{Task, TaskStatus};

use super::super::lifecycle::task_status_transition_allowed;
use super::{enter_isolated_child, test_runtime};
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
    if !enter_isolated_child(
        module_path!(),
        "guarded_update_writes_every_legal_edge_and_refuses_every_other_one",
    ) {
        return;
    }
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
