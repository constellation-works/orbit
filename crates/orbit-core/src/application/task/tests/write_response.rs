//! Dashboard triage is decoration: an unreadable job store does not fail the task.

use orbit_types::task::TaskStatus;

use super::{enter_isolated_child, test_runtime};
use crate::application::task::write_response::{
    DashboardPageReads, TaskWriteResponsePolicy, task_detail_with_dashboard_extras,
    task_write_response_json,
};
use crate::application::task::{TaskAddParams, TaskWriteMetadata};

#[test]
fn unreadable_job_runs_leave_dashboard_task_detail_without_triage() {
    if !enter_isolated_child(
        module_path!(),
        "unreadable_job_runs_leave_dashboard_task_detail_without_triage",
    ) {
        return;
    }
    let (_root, runtime) = test_runtime();
    let task = runtime
        .add_task(TaskAddParams {
            title: "Dashboard task".into(),
            description: "Shown without triage badges.".into(),
            acceptance_criteria: vec!["The record still loads.".into()],
            status: Some(TaskStatus::Proposed),
            ..Default::default()
        })
        .expect("add task");
    let database = runtime.global_root().join("orbit.db");
    let connection = rusqlite::Connection::open(&database).expect("open store");
    connection
        .busy_timeout(std::time::Duration::from_secs(5))
        .expect("busy timeout");
    connection
        .execute_batch("ALTER TABLE job_runs RENAME TO job_runs_unreadable;")
        .expect("make job-run listing fail");

    let row = runtime
        .task_row_with_sidecars(task.clone())
        .expect("task row");
    let detail = task_detail_with_dashboard_extras(
        &runtime,
        &row,
        &Default::default(),
        &DashboardPageReads::default(),
    )
    .expect("detail degrades when job runs cannot be listed");
    assert!(
        detail.get("active_triage").is_none(),
        "no triage binding is invented: {detail}"
    );
    assert_eq!(detail["id"].as_str(), Some(task.id.as_str()));

    let response = task_write_response_json(
        &runtime,
        &task,
        &TaskWriteResponsePolicy::DashboardFullRecord,
        &TaskWriteMetadata::default(),
    )
    .expect("a dashboard write response degrades the same way");
    assert!(response.get("active_triage").is_none(), "{response}");
    assert_eq!(response["id"].as_str(), Some(task.id.as_str()));
}
