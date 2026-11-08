//! Template edits merge with the committed definition inside the cursor lock.

use std::sync::Arc;
use std::sync::mpsc::{RecvTimeoutError, sync_channel};
use std::time::Duration;

use orbit_common::fs::io::with_exclusive_file_lock;
use orbit_core::application::auto_tasks::{
    AutoTaskScheduleError, AutoTaskScheduleOutcome, AutoTaskScheduleProfile, AutoTaskTemplatePatch,
    AutoTaskUpdateParams, cursor_state_path,
};
use orbit_core::{
    AutoTaskAddParams, AutoTaskSchedule, AutoTaskTemplate, DedupePolicy, OrbitError, OrbitRuntime,
    TaskComplexity, TaskPriority, TaskStatus, TaskType,
};

#[test]
fn waiting_template_edit_preserves_a_different_field_committed_under_the_lock() {
    if !super::dispatch_admission::isolated(
        "auto_task_update::waiting_template_edit_preserves_a_different_field_committed_under_the_lock",
    ) {
        return;
    }
    let runtime = Arc::new(OrbitRuntime::in_memory().unwrap());
    let original = runtime
        .auto_task_add(AutoTaskAddParams {
            name: "concurrent-template".into(),
            description: "Concurrent template edits".into(),
            schedule: AutoTaskSchedule::every_minutes(60),
            template: AutoTaskTemplate {
                title: "Original title".into(),
                description: "Keep this body".into(),
                acceptance_criteria: vec!["Retain unnamed template fields".into()],
                task_type: TaskType::Chore,
                tags: vec!["keep-tag".into()],
                required_tools: vec!["orbit.task.show".into()],
                context_files: vec!["file:README.md".into()],
                priority: TaskPriority::Medium,
                complexity: Some(TaskComplexity::Low),
                crew: None,
                status: TaskStatus::Backlog,
            },
            dedupe: DedupePolicy::SkipIfOpen,
        })
        .unwrap();
    // Prepare the title edit before another writer commits its priority edit.
    let title_edit = AutoTaskUpdateParams {
        template: Some(AutoTaskTemplatePatch {
            title: Some("Updated title".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    let (started_tx, started_rx) = sync_channel(1);
    let (done_tx, done_rx) = sync_channel(1);
    let worker = with_exclusive_file_lock(
        &cursor_state_path(&runtime.paths().state_dir),
        "auto-task cursor",
        || {
            let editor = Arc::clone(&runtime);
            let worker = std::thread::spawn(move || {
                started_tx.send(()).unwrap();
                done_tx
                    .send(editor.auto_task_update("concurrent-template", title_edit))
                    .unwrap();
            });
            started_rx.recv_timeout(Duration::from_secs(30)).unwrap();
            assert!(
                matches!(
                    done_rx.recv_timeout(Duration::from_secs(1)),
                    Err(RecvTimeoutError::Timeout)
                ),
                "the title edit must wait for the cursor lock"
            );
            // Re-enter the held lock to commit while the title editor waits.
            let priority_edit = runtime
                .auto_task_update(
                    "concurrent-template",
                    AutoTaskUpdateParams {
                        template: Some(AutoTaskTemplatePatch {
                            priority: Some(TaskPriority::High),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                )
                .unwrap();
            assert_eq!(priority_edit.template.priority, TaskPriority::High);
            Ok::<_, OrbitError>(worker)
        },
    )
    .unwrap();
    let edited = done_rx
        .recv_timeout(Duration::from_secs(30))
        .unwrap()
        .unwrap();
    worker.join().unwrap();

    let mut expected = original.template;
    expected.title = "Updated title".into();
    expected.priority = TaskPriority::High;
    assert_eq!(edited.template, expected);
    assert_eq!(
        runtime
            .auto_task_show("concurrent-template")
            .unwrap()
            .unwrap()
            .template,
        expected,
        "both independent edits must survive in the persisted definition"
    );
}

fn schedule_fixture(name: &str) -> OrbitRuntime {
    let runtime = OrbitRuntime::in_memory().unwrap();
    runtime
        .auto_task_add(AutoTaskAddParams {
            name: name.into(),
            description: "Schedule edits".into(),
            schedule: AutoTaskSchedule::every_minutes(60),
            template: AutoTaskTemplate {
                title: "Scheduled".into(),
                description: "Body".into(),
                acceptance_criteria: vec!["Done".into()],
                task_type: TaskType::Chore,
                tags: Vec::new(),
                required_tools: Vec::new(),
                context_files: Vec::new(),
                priority: TaskPriority::Medium,
                complexity: Some(TaskComplexity::Low),
                crew: None,
                status: TaskStatus::Backlog,
            },
            dedupe: DedupePolicy::SkipIfOpen,
        })
        .unwrap();
    runtime
}

#[test]
fn schedule_command_profiles_compare_no_op_and_stamp_as_specified() {
    if !super::dispatch_admission::isolated(
        "auto_task_update::schedule_command_profiles_compare_no_op_and_stamp_as_specified",
    ) {
        return;
    }
    let runtime = schedule_fixture("scheduled");
    let current = AutoTaskSchedule::every_minutes(60);
    let next = AutoTaskSchedule::every_minutes(30);
    let observed = |expected_schedule: &AutoTaskSchedule| AutoTaskScheduleProfile::Observed {
        expected_schedule: Box::new(expected_schedule.clone()),
    };
    let stamp = |runtime: &OrbitRuntime| {
        runtime
            .auto_task_show("scheduled")
            .unwrap()
            .unwrap()
            .updated_at
    };
    let before = stamp(&runtime);

    assert!(matches!(
        runtime
            .auto_task_schedule_command("missing", next.clone(), observed(&current))
            .unwrap(),
        AutoTaskScheduleOutcome::NotFound
    ));
    match runtime
        .auto_task_schedule_command("scheduled", next.clone(), observed(&next))
        .unwrap()
    {
        AutoTaskScheduleOutcome::Stale { actual_schedule } => {
            assert_eq!(*actual_schedule, current);
        }
        other => panic!("expected a stale outcome, got {other:?}"),
    }
    assert!(matches!(
        runtime
            .auto_task_schedule_command("scheduled", current.clone(), observed(&current))
            .unwrap(),
        AutoTaskScheduleOutcome::AlreadySet(_)
    ));
    assert_eq!(stamp(&runtime), before, "refusals and no-ops never stamp");

    let retired: AutoTaskSchedule = serde_json::from_value(serde_json::json!({
        "deliveries_landed": {
            "owner_machine": "fixture-machine",
            "branch": "agent-main",
            "threshold": 2,
            "max_wait_minutes": 60,
            "coverage": "integrated_qa_v1",
            "max_items": 2,
            "retries": 1
        }
    }))
    .unwrap();
    let refused = runtime
        .auto_task_schedule_command("scheduled", retired, observed(&current))
        .unwrap_err();
    assert!(
        matches!(&refused, AutoTaskScheduleError::Write(OrbitError::InvalidInput(message))
        if message.contains("integrated_qa_v1"))
    );

    assert!(matches!(
        runtime
            .auto_task_schedule_command("scheduled", next.clone(), observed(&current))
            .unwrap(),
        AutoTaskScheduleOutcome::Written(written) if written.schedule == next
    ));
    let written_stamp = stamp(&runtime);
    assert_ne!(written_stamp, before, "a real change stamps");

    // The ordinary profile writes and stamps even when nothing changes.
    assert!(matches!(
        runtime
            .auto_task_schedule_command(
                "scheduled",
                next.clone(),
                AutoTaskScheduleProfile::Unconditional,
            )
            .unwrap(),
        AutoTaskScheduleOutcome::Written(_)
    ));
    assert_ne!(stamp(&runtime), written_stamp);
}

#[test]
fn compound_update_writes_schedule_and_other_fields_in_one_edit() {
    if !super::dispatch_admission::isolated(
        "auto_task_update::compound_update_writes_schedule_and_other_fields_in_one_edit",
    ) {
        return;
    }
    let runtime = schedule_fixture("compound");
    let edited = runtime
        .auto_task_update(
            "compound",
            AutoTaskUpdateParams {
                description: Some("Edited together".into()),
                schedule: Some(AutoTaskSchedule::every_minutes(15)),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(edited.description, "Edited together");
    assert_eq!(edited.schedule, AutoTaskSchedule::every_minutes(15));
}
