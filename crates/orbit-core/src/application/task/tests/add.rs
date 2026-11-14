use orbit_common::OrbitError;

use crate::application::task::{TaskAddParams, TaskUpdateParams};

use super::{enter_isolated_child, test_runtime};

/// Fault injection: the crew provenance write after the insert fails once.
/// The insert has committed, so every create must report that task,
/// and no retry may insert a second one.
#[test]
fn creation_provenance_failure_keeps_one_task() {
    if !enter_isolated_child(module_path!(), "creation_provenance_failure_keeps_one_task") {
        return;
    }
    let (_root, runtime) = test_runtime();
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
            runtime.list_tasks().expect("tasks").len(),
            before + 1,
            "{key:?}: the failed provenance write leaves exactly one task"
        );

        if key.is_some() {
            let retry = runtime
                .add_task_admitted_guarded(params, None, None, key, digest)
                .expect("keyed retry replays");
            assert_eq!(retry.id, id, "{key:?}: the retry returns the original task");
            assert_eq!(runtime.list_tasks().expect("tasks").len(), before + 1);
        }
    }
}

// --- ORB-00251: context_files omission / over-inclusion warning helper tests ---

/// Fault injection: the mint's reply is lost, then an operator changes the
/// crew before another retry. The internal keyed admission seam is not
/// exposed by ordinary task-add transports.
#[test]
fn keyed_creation_lost_reply_replays_without_history_changes() {
    if !enter_isolated_child(
        module_path!(),
        "keyed_creation_lost_reply_replays_without_history_changes",
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
    }
    assert_eq!(runtime.list_tasks().expect("tasks").len(), 2);
}

fn seed_implement_activity(runtime: &crate::OrbitRuntime) -> std::path::PathBuf {
    let dir = runtime.global_root().join("resources/activities");
    std::fs::create_dir_all(&dir).expect("activity dir");
    let dest = dir.join("agent_implement.yaml");
    std::fs::copy(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("assets/activities/agent_implement.yaml"),
        &dest,
    )
    .expect("seed agent_implement");
    dest
}

fn assert_unliftable_refusal(message: &str, tool: &str) {
    assert!(message.contains(tool), "{message}");
    assert!(message.contains("agent_implement"), "{message}");
    assert!(message.contains("disallow list"), "{message}");
    assert!(
        message.contains("operator") && message.contains("separate operator-owned task"),
        "{message}"
    );
}

/// An allowed tool is a requirement the implementing activity can call.
#[test]
fn filing_accepts_an_allowed_required_tool() {
    if !enter_isolated_child(module_path!(), "filing_accepts_an_allowed_required_tool") {
        return;
    }
    let (_root, runtime) = test_runtime();
    seed_implement_activity(&runtime);
    let task = runtime
        .add_task(TaskAddParams {
            title: "Allowed requirement".into(),
            required_tools: vec!["orbit.task.show".into()],
            ..Default::default()
        })
        .expect("an allowed tool is a valid requirement");
    assert_eq!(task.required_tools, ["orbit.task.show"]);
}

/// A disallowed mutating tool cannot be required: no grant would run it live.
#[test]
fn filing_refuses_an_unliftable_disallowed_required_tool() {
    if !enter_isolated_child(
        module_path!(),
        "filing_refuses_an_unliftable_disallowed_required_tool",
    ) {
        return;
    }
    let (_root, runtime) = test_runtime();
    seed_implement_activity(&runtime);
    let error = runtime
        .add_task(TaskAddParams {
            title: "Disallowed mutating requirement".into(),
            required_tools: vec!["orbit.auto_task:update".into()],
            ..Default::default()
        })
        .expect_err("a disallowed mutating tool is refused at filing");
    let message = error.to_string();
    assert_unliftable_refusal(&message, "orbit.auto_task:update");
    assert!(message.contains("mutating"), "{message}");
    assert!(
        runtime.list_tasks().expect("tasks").is_empty(),
        "a refused requirement is not stored"
    );
}

/// An exact denial of a read/repeat-safe tool can be required, and granted.
#[test]
fn filing_accepts_a_disallowed_read_only_tool_a_grant_can_lift() {
    if !enter_isolated_child(
        module_path!(),
        "filing_accepts_a_disallowed_read_only_tool_a_grant_can_lift",
    ) {
        return;
    }
    let (_root, runtime) = test_runtime();
    let activity = seed_implement_activity(&runtime);
    let text = std::fs::read_to_string(&activity).expect("read activity");
    let needle = "  tool_disallow_list:\n";
    assert!(
        text.contains(needle),
        "shipped agent_implement must declare a disallow list"
    );
    std::fs::write(
        &activity,
        text.replacen(needle, "  tool_disallow_list:\n    - orbit.task.show\n", 1),
    )
    .expect("deny orbit.task.show");

    let refused = runtime
        .add_task(TaskAddParams {
            title: "Denied until classified read-only".into(),
            required_tools: vec!["orbit.task.show".into()],
            ..Default::default()
        })
        .expect_err("the default classification of an unclassified tool is mutating");
    assert_unliftable_refusal(&refused.to_string(), "orbit.task.show");

    let _classification = crate::application::job::set_grant_classification_override(
        "orbit.task.show",
        orbit_common::governance::authorization::ToolGrantClassification {
            grantability: orbit_common::governance::authorization::ToolGrantability::Grantable,
            effect: orbit_common::governance::authorization::GrantedToolEffect::ReadRepeatSafe,
        },
    );
    let task = runtime
        .add_task(TaskAddParams {
            title: "Liftable read-only requirement".into(),
            required_tools: vec!["orbit.task.show".into()],
            granted_tools: vec!["orbit.task.show".into()],
            ..Default::default()
        })
        .expect("a grant can lift an exact read/repeat-safe denial");
    assert_eq!(task.required_tools, ["orbit.task.show"]);
    assert_eq!(task.granted_tools, ["orbit.task.show"]);
}

/// A malformed global activity file must not block filing a task with no
/// `required_tools`, while a task naming a requirement still fails closed
/// with the catalog error and stores nothing. [ORB-23413]
#[test]
fn filing_without_required_tools_survives_a_malformed_global_activity() {
    if !enter_isolated_child(
        module_path!(),
        "filing_without_required_tools_survives_a_malformed_global_activity",
    ) {
        return;
    }
    let (_root, runtime) = test_runtime();
    seed_implement_activity(&runtime);
    let broken = runtime
        .global_root()
        .join("resources/activities/malformed-activity.yaml");
    std::fs::write(&broken, "{{{\n  not: [valid, yaml\n").expect("write malformed activity");

    let task = runtime
        .add_task(TaskAddParams {
            title: "No requirements to check".into(),
            ..Default::default()
        })
        .expect("an unreadable catalog must not block filing without required_tools");
    assert!(
        task.required_tools.is_empty(),
        "the filed task carries no requirements"
    );

    let before = runtime.list_tasks().expect("tasks").len();
    let error = runtime
        .add_task(TaskAddParams {
            title: "Requirement still needs the catalog".into(),
            required_tools: vec!["orbit.task.show".into()],
            ..Default::default()
        })
        .expect_err("a requirement cannot be checked while the catalog fails to load");
    let message = error.to_string();
    assert!(
        message.contains("malformed-activity.yaml"),
        "the catalog error names the file: {message}"
    );
    assert_eq!(
        runtime.list_tasks().expect("tasks").len(),
        before,
        "a refused requirement is not stored"
    );
}
