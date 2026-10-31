use super::*;

#[test]
fn delete_rechecks_a_status_changed_after_its_guard_and_preserves_force() {
    if !enter_isolated_child(
        module_path!(),
        "delete_rechecks_a_status_changed_after_its_guard_and_preserves_force",
    ) {
        return;
    }
    let (_root, runtime) = test_runtime();

    for status in [TaskStatus::Proposed, TaskStatus::Rejected] {
        let task = add_task(&runtime, "Delete stale guard");
        if status != TaskStatus::Proposed {
            set_status(&runtime, &task.id, status);
        }
        // Commit a competing transition after deletion's initial allowed
        // check, before it acquires the task write lock.
        runtime.set_transition_read_hook_status(Some(&task.id), Some(TaskStatus::InProgress));
        let result = runtime.delete_task_guarded(&task.id, false);
        runtime.set_transition_read_hook_status(None, None);

        assert!(
            matches!(result, Err(orbit_common::OrbitError::InvalidInput(_))),
            "a stale deletion guard must refuse the newer status: {result:?}"
        );
        assert_eq!(
            runtime
                .get_task(&task.id)
                .expect("task must survive")
                .status,
            TaskStatus::InProgress
        );

        runtime
            .delete_task_guarded(&task.id, true)
            .expect("force permits deletion of the newer status");
        assert!(matches!(
            runtime.get_task(&task.id),
            Err(orbit_common::OrbitError::NotFound { .. })
        ));

        let allowed = add_task(&runtime, "Delete allowed status");
        if status != TaskStatus::Proposed {
            set_status(&runtime, &allowed.id, status);
        }
        runtime
            .delete_task_guarded(&allowed.id, false)
            .expect("unchanged proposed or rejected tasks remain deletable");
        assert!(matches!(
            runtime.get_task(&allowed.id),
            Err(orbit_common::OrbitError::NotFound { .. })
        ));
    }
}
