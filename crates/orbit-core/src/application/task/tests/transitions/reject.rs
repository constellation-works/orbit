use super::*;

#[test]
fn dashboard_archive_rechecks_run_ownership_after_its_guard_and_preserves_force() {
    if !enter_isolated_child(
        module_path!(),
        "dashboard_archive_rechecks_run_ownership_after_its_guard_and_preserves_force",
    ) {
        return;
    }
    let (_root, runtime) = test_runtime();
    let task = add_task(&runtime, "Archive stale guard");

    // A local run starts owning the task after the dashboard archive's outer
    // ownership pre-check and before the archive write commits.
    runtime.set_transition_run_hook_task(Some(&task.id));
    let result = runtime.archive_task_for_dashboard(&task.id, false);
    runtime.set_transition_run_hook_task(None);

    let refusal = result
        .expect_err("a run inserted after the guard must refuse the archive")
        .force_overridable()
        .cloned()
        .expect("the refusal carries the force override");
    assert_eq!(
        refusal.kind,
        orbit_common::ForceOverridableKind::RunStillOwnsTask,
        "the archive refusal names its run-ownership guard"
    );
    let run_id = refusal
        .run_id
        .clone()
        .expect("the refusal names the owning run");
    assert_eq!(
        runtime
            .get_task(&task.id)
            .expect("task must survive")
            .status,
        TaskStatus::Proposed,
        "a refused archive writes nothing"
    );

    runtime
        .archive_task_for_dashboard(&task.id, true)
        .expect("force archives despite the owning run");
    assert_eq!(
        runtime.get_task(&task.id).expect("archived task").status,
        TaskStatus::Archived
    );
    assert!(
        runtime
            .get_task_history(&task.id)
            .expect("history")
            .iter()
            .any(|entry| entry.event == "forced"),
        "the forced archive is recorded as forced"
    );
    assert!(
        !runtime
            .show_job_run(&run_id)
            .expect("owning run")
            .state
            .is_terminal(),
        "forcing does not cancel the owning run"
    );
}

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
            matches!(result, Err(orbit_common::OrbitError::ForceOverridable(_))),
            "a stale deletion guard must refuse the newer status: {result:?}"
        );
        assert_eq!(
            result
                .unwrap_err()
                .force_overridable()
                .map(|refusal| refusal.kind),
            Some(orbit_common::ForceOverridableKind::DeleteOutsideProposedOrRejected),
            "the delete refusal carries its guard kind"
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
