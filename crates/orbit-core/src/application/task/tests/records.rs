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

#[test]
fn concurrent_status_change_cannot_split_a_cas_guarded_write() {
    if !enter_isolated_child(
        module_path!(),
        "concurrent_status_change_cannot_split_a_cas_guarded_write",
    ) {
        return;
    }
    use std::sync::mpsc;
    use std::time::Duration;

    let (_root, runtime) = test_runtime();
    let task = runtime
        .add_task(TaskAddParams {
            title: "Original title".into(),
            description: "A task whose status a concurrent writer moves.".into(),
            acceptance_criteria: vec!["The CAS write applies whole or not at all.".into()],
            ..Default::default()
        })
        .expect("add task");
    let backlog = runtime.get_task(&task.id).expect("task").status;

    let (go_tx, go_rx) = mpsc::channel::<()>();
    let (done_tx, done_rx) = mpsc::channel();
    // Between the write's companion commits and its history commit, start a
    // concurrent status change on another thread and give it the chance to
    // land before the history commit re-checks the CAS.
    let raced_inside = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let raced = raced_inside.clone();
    super::super::records::BEFORE_HISTORY_COMMIT.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            go_tx.send(()).expect("start racer");
            if done_rx.recv_timeout(Duration::from_secs(2)).is_ok() {
                raced.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }));
    });

    let result = std::thread::scope(|scope| {
        let task_id = &task.id;
        let runtime = &runtime;
        let racer = scope.spawn(move || {
            go_rx.recv().expect("racer signal");
            let moved = runtime.stores().task_records().update(
                task_id,
                TaskRecordUpdateParams {
                    actor: "racer".into(),
                    status: Some(TaskStatus::Blocked),
                    ..Default::default()
                },
            );
            let _ = done_tx.send(());
            moved
        });
        let result = runtime.stores().task_records().update(
            &task.id,
            TaskRecordUpdateParams {
                actor: "test".into(),
                title: Some("Changed title".into()),
                status_note: Some("guarded write".into()),
                upsert_artifacts: vec![TaskArtifact::from_text("cas-evidence.txt", "evidence")],
                expected_status: Some(vec![backlog]),
                ..Default::default()
            },
        );
        racer.join().expect("racer thread").expect("racer write");
        result
    });

    let after = runtime.get_task(&task.id).expect("task");
    let artifact = runtime
        .get_task_artifact(&task.id, "cas-evidence.txt")
        .expect("artifact read");
    match &result {
        Ok(_) => {
            assert_eq!(after.title, "Changed title");
            assert!(artifact.is_some(), "a successful write keeps its artifact");
        }
        Err(_) => {
            assert_eq!(
                after.title, "Original title",
                "CAS failure left a document edit"
            );
            assert!(artifact.is_none(), "CAS failure left an artifact behind");
        }
    }
    assert!(
        !raced_inside.load(std::sync::atomic::Ordering::SeqCst),
        "a status change landed between the CAS pre-check and the history commit: {result:?}"
    );
    assert_eq!(
        after.status,
        TaskStatus::Blocked,
        "the racer serialized after"
    );
}
