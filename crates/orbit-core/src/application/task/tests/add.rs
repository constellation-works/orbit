use orbit_common::OrbitError;
use orbit_types::record::OrbitEvent;

use crate::application::task::{TaskAddParams, TaskUpdateParams};

use super::{enter_isolated_child, test_runtime};

/// Fault injection: the crew provenance write after the insert fails once.
/// The insert has committed, so every create must report that task and
/// publish its event, and no retry may insert a second one.
#[test]
fn creation_provenance_failure_keeps_one_task_and_its_event() {
    if !enter_isolated_child(
        module_path!(),
        "creation_provenance_failure_keeps_one_task_and_its_event",
    ) {
        return;
    }
    let (_root, runtime) = test_runtime();
    let task_added = |id: &str| {
        runtime
            .event_log
            .snapshot()
            .iter()
            .filter(|event| matches!(event, OrbitEvent::TaskAdded { id: added } if added == id))
            .count()
    };
    let crew_history = |id: &str| {
        runtime
            .get_task_history(id)
            .expect("creation history")
            .iter()
            .filter(|entry| entry.event == "crew_assigned")
            .count()
    };
    let fail_next_update = || super::super::records::FAIL_NEXT_UPDATE.with(|fail| fail.set(true));

    for (key, digest) in [
        (None, None),
        (Some("automation-mint"), None),
        (Some("desktop-create"), Some("payload")),
    ] {
        let params = TaskAddParams {
            title: format!("Provenance failure for {key:?}"),
            system_created: true,
            ..Default::default()
        };
        let before = runtime.list_tasks().expect("tasks").len();
        fail_next_update();
        let id = match runtime.add_task_admitted_guarded(params.clone(), None, None, key, digest) {
            Ok(task) => task.id,
            Err(OrbitError::DesktopWriteAccepted { task_id, .. }) if digest.is_some() => task_id,
            Err(error) => panic!("{key:?}: the committed create must be reported: {error}"),
        };
        let task = runtime.get_task(&id).expect("the reported task exists");
        assert!(task.crew.is_some(), "{key:?}: the create drew a crew");
        assert_eq!(
            crew_history(&id),
            0,
            "{key:?}: the injected fault hit provenance"
        );
        assert_eq!(
            task_added(&id),
            1,
            "{key:?}: the committed insert emits TaskAdded"
        );
        assert_eq!(
            runtime.list_tasks().expect("tasks").len(),
            before + 1,
            "{key:?}: the failed provenance write leaves exactly one task"
        );

        if key.is_some() {
            let retry = runtime
                .add_task_admitted_guarded(params, None, None, key, digest)
                .expect("keyed retry replays");
            assert_eq!(retry.id, id, "{key:?}: the retry returns the original task");
            assert_eq!(task_added(&id), 1, "{key:?}: a replay emits no event");
            assert_eq!(runtime.list_tasks().expect("tasks").len(), before + 1);
        }
    }
}

// --- ORB-00251: context_files omission / over-inclusion warning helper tests ---

/// Fault injection: the mint's reply is lost, then an operator changes the
/// crew before another retry. The internal keyed admission seam is not
/// exposed by ordinary task-add transports.
#[test]
fn keyed_creation_lost_reply_replays_without_history_or_events() {
    if !enter_isolated_child(
        module_path!(),
        "keyed_creation_lost_reply_replays_without_history_or_events",
    ) {
        return;
    }
    let (_root, runtime) = test_runtime();
    for (key, digest) in [
        ("automation-mint", None),
        ("desktop-create", Some("payload")),
    ] {
        let params = TaskAddParams {
            title: "Retry a keyed creation".to_string(),
            system_created: true,
            ..Default::default()
        };
        let create = || {
            runtime
                .add_task_admitted_guarded(params.clone(), None, None, Some(key), digest)
                .expect("keyed creation succeeds")
        };
        let task = create();
        let history = runtime
            .get_task_history(&task.id)
            .expect("creation history");
        assert_eq!(
            history
                .iter()
                .filter(|entry| entry.event == "crew_assigned")
                .count(),
            1,
            "ORB-14352: creation records crew provenance once"
        );
        let replay = create();
        assert_eq!(replay, task, "replay preserves the task and its revision");
        assert_eq!(
            runtime
                .get_task_history(&task.id)
                .expect("replayed history"),
            history,
            "ORB-14352: a lost-reply retry appends no history"
        );

        let reassigned = runtime
            .update_task(
                &task.id,
                TaskUpdateParams {
                    crew: Some(Some("orchestration".to_string())),
                    ..Default::default()
                },
            )
            .expect("operator reassigns crew");
        let history = runtime
            .get_task_history(&task.id)
            .expect("operator history");
        assert_eq!(
            create(),
            reassigned,
            "replay retains the operator's crew and revision"
        );
        assert_eq!(
            runtime.get_task_history(&task.id).expect("final history"),
            history,
            "replay must not append stale creation provenance"
        );
        assert_eq!(
            runtime
                .event_log
                .snapshot()
                .iter()
                .filter(|event| { matches!(event, OrbitEvent::TaskAdded { id } if id == &task.id) })
                .count(),
            1,
            "ORB-14352: only the first creation emits TaskAdded"
        );
    }
    assert_eq!(runtime.list_tasks().expect("tasks").len(), 2);
}
