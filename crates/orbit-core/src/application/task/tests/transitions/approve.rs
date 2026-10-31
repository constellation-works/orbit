use super::*;

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
