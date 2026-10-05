//! Human task transitions retain the status they read when they write.

use orbit_types::task::TaskStatus;

use super::{enter_isolated_child, test_runtime};
use crate::application::task::{TaskAddParams, TaskUpdateParams};

fn add_task(runtime: &crate::OrbitRuntime, title: &str) -> orbit_types::task::Task {
    runtime
        .add_task(TaskAddParams {
            title: title.to_string(),
            description: "Exercise transition compare-and-set behavior.".to_string(),
            acceptance_criteria: vec![
                "The transition must not overwrite a newer status.".to_string(),
            ],
            plan: "Use the transition lock.".to_string(),
            ..Default::default()
        })
        .expect("add task")
}

fn set_status(runtime: &crate::OrbitRuntime, id: &str, status: TaskStatus) {
    runtime
        .update_task(
            id,
            TaskUpdateParams {
                status: Some(status),
                ..Default::default()
            },
        )
        .expect("set status through the store");
}

fn with_changed_status(runtime: &crate::OrbitRuntime, id: &str, test: impl FnOnce()) {
    runtime.set_transition_read_hook_status(Some(id), Some(TaskStatus::Rejected));
    test();
    runtime.set_transition_read_hook_status(None, None);
}

#[test]
fn approve_does_not_overwrite_a_status_changed_after_its_read() {
    if !enter_isolated_child(
        module_path!(),
        "approve_does_not_overwrite_a_status_changed_after_its_read",
    ) {
        return;
    }
    let (_root, runtime) = test_runtime();
    let task = add_task(&runtime, "Approve CAS");
    set_status(&runtime, &task.id, TaskStatus::Review);

    with_changed_status(&runtime, &task.id, || {
        let error = runtime
            .approve_task(&task.id, None, None)
            .expect_err("approve must lose to the direct store update");
        assert!(
            error.to_string().contains("status changed to 'rejected'"),
            "{error}"
        );
    });

    assert_eq!(
        runtime.get_task(&task.id).expect("current task").status,
        TaskStatus::Rejected
    );
}
