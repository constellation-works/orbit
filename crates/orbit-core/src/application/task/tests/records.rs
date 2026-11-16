//! A multi-part write carrying `expected_status` applies under the status
//! compare-and-set or not at all.

use orbit_types::task::{TaskArtifact, TaskStatus};

use super::{enter_isolated_child, test_runtime};
use crate::application::task::TaskAddParams;
use crate::application::task::params::TaskRecordUpdateParams;
use crate::application::task::records::TaskRecords;

#[test]
fn cas_failure_leaves_no_companion_edits_behind() {
    if !enter_isolated_child(
        module_path!(),
        "cas_failure_leaves_no_companion_edits_behind",
    ) {
        return;
    }
    let (_root, runtime) = test_runtime();
    let task = runtime
        .add_task(TaskAddParams {
            title: "Original title".into(),
            description: "A task whose status moved first.".into(),
            acceptance_criteria: vec!["No companion edit survives a CAS failure.".into()],
            ..Default::default()
        })
        .expect("add task");
    let history_before = runtime.get_task_history(&task.id).expect("history").len();

    // Document, artifact and history changes in one write whose status CAS
    // cannot hold: the task is in the backlog, not done.
    let refused = runtime.stores().task_records().update(
        &task.id,
        TaskRecordUpdateParams {
            actor: "test".into(),
            title: Some("Changed title".into()),
            status: Some(TaskStatus::Blocked),
            upsert_artifacts: vec![TaskArtifact::from_text("cas-evidence.txt", "evidence")],
            expected_status: Some(vec![TaskStatus::Done]),
            ..Default::default()
        },
    );
    assert!(refused.is_err(), "{refused:?}");

    let after = runtime.get_task(&task.id).expect("task");
    assert_eq!(after.title, "Original title");
    assert_eq!(after.status, task.status);
    assert_eq!(
        runtime.get_task_history(&task.id).expect("history").len(),
        history_before,
        "no history entry landed"
    );
    assert!(
        runtime
            .get_task_artifact(&task.id, "cas-evidence.txt")
            .expect("artifact read")
            .is_none(),
        "no artifact landed"
    );

    // An artifact-only write never reaches a history commit, but the CAS
    // still guards it.
    let refused = runtime.stores().task_records().update(
        &task.id,
        TaskRecordUpdateParams {
            actor: "test".into(),
            upsert_artifacts: vec![TaskArtifact::from_text("cas-evidence.txt", "evidence")],
            expected_status: Some(vec![TaskStatus::Done]),
            ..Default::default()
        },
    );
    assert!(refused.is_err(), "{refused:?}");
    assert!(
        runtime
            .get_task_artifact(&task.id, "cas-evidence.txt")
            .expect("artifact read")
            .is_none(),
        "an artifact-only CAS failure lands nothing"
    );
}
