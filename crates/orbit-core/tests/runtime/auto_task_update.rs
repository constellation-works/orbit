//! Template edits merge with the committed definition inside the cursor lock.

use std::sync::Arc;
use std::sync::mpsc::{RecvTimeoutError, sync_channel};
use std::time::Duration;

use orbit_common::fs::io::with_exclusive_file_lock;
use orbit_core::application::auto_tasks::cursor_state_path;
use orbit_core::{
    AutoTaskAddParams, AutoTaskSchedule, AutoTaskTemplate, AutoTaskTemplatePatch,
    AutoTaskUpdateParams, DedupePolicy, OrbitError, OrbitRuntime, TaskComplexity, TaskPriority,
    TaskStatus, TaskType,
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
            schedule: AutoTaskSchedule::Interval { every_minutes: 60 },
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
