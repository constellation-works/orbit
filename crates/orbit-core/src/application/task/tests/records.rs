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
            status: Some(TaskStatus::InProgress),
            ..Default::default()
        })
        .expect("add task");

    // The cfg(test) hook only orders the interleaving: it fires between the
    // guarded write's document commit and its history commit, starting a
    // concurrent public status change and giving it the chance to land
    // before the history commit re-checks the CAS. Both writes themselves
    // go through the public activity entry point, with its production
    // authority (system actor, internal status authority) and input shape
    // (`TaskActivityUpdate` carrying `expected_status`).
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let (done_tx, done_rx) = mpsc::channel();
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
            let moved = runtime.update_task_from_activity(
                task_id,
                orbit_engine::TaskActivityUpdate {
                    status: TaskStatus::Blocked,
                    expected_status: TaskStatus::InProgress,
                    execution_summary: None,
                    comment: None,
                    note: Some("racer".into()),
                    agent: None,
                    model: None,
                    calling_run_id: None,
                },
            );
            let _ = done_tx.send(());
            moved
        });
        let result = runtime.update_task_from_activity(
            &task.id,
            orbit_engine::TaskActivityUpdate {
                status: TaskStatus::Review,
                expected_status: TaskStatus::InProgress,
                execution_summary: Some("guarded summary".into()),
                comment: Some("guarded comment".into()),
                note: Some("guarded write".into()),
                agent: None,
                model: None,
                calling_run_id: None,
            },
        );
        let _ = racer.join().expect("racer thread");
        result
    });

    let after = runtime.get_task(&task.id).expect("task");
    let comments = runtime.get_task_comments(&task.id).expect("comments");
    match &result {
        Ok(_) => {
            assert_eq!(after.status, TaskStatus::Review);
            assert_eq!(after.execution_summary, "guarded summary");
            assert!(
                comments.iter().any(|c| c.message == "guarded comment"),
                "a successful write keeps its comment"
            );
        }
        Err(_) => {
            assert_eq!(
                after.execution_summary, "",
                "CAS failure left a document edit: {after:?}"
            );
            assert!(
                !comments.iter().any(|c| c.message == "guarded comment"),
                "CAS failure left a comment behind"
            );
        }
    }
    assert!(
        !raced_inside.load(std::sync::atomic::Ordering::SeqCst),
        "a status change landed between the CAS pre-check and the history commit: {result:?}"
    );
}
