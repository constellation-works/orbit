//! Human task transitions and deletion guard against stale status reads.

use orbit_types::task::TaskStatus;

use super::{enter_isolated_child, test_runtime};
use crate::application::task::{TaskAddParams, TaskUpdateParams};

pub(super) fn add_task(runtime: &crate::OrbitRuntime, title: &str) -> orbit_types::task::Task {
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

pub(super) fn set_status(runtime: &crate::OrbitRuntime, id: &str, status: TaskStatus) {
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

pub(super) fn with_changed_status(runtime: &crate::OrbitRuntime, id: &str, test: impl FnOnce()) {
    runtime.set_transition_read_hook_status(Some(id), Some(TaskStatus::Rejected));
    test();
    runtime.set_transition_read_hook_status(None, None);
}

mod approve;
mod reject;
