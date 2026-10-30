//! Admission identity survives metadata edits across delivery boundaries.

use super::*;

/// ORB-19929: scope metadata must not rename the crew of an admitted delivery.
#[test]
fn admitted_scope_and_complexity_edits_preserve_execution_identity() {
    if !isolated(
        "task_pilot::crew_selection::admitted_scope::admitted_scope_and_complexity_edits_preserve_execution_identity",
    ) {
        return;
    }
    let workspace = workspace();
    let task = task(&workspace, false);
    install_delivery_job(&workspace);
    let held_worker = hold_delivery_worker();
    let admitted = workspace
        .runtime
        .submit_pipeline_run(
            "task_local_pipeline",
            json!({"task_id": task.id}),
            None,
            Some("fixture"),
        )
        .unwrap();
    held_worker.wait_for_reap();
    let crew = workspace
        .runtime
        .resolve_crew_for_task(None, task.crew.as_deref())
        .unwrap();
    workspace
        .jobs
        .record_job_run_crew(&admitted.run_id, &crew)
        .unwrap();
    let before = workspace
        .jobs
        .get_job_run(&admitted.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        before.input.as_ref().unwrap()["crew_selection"]["crew"],
        "low_lane"
    );
    // The run reserves the backlog task even before worktree_setup binds it.
    let updated = workspace
        .runtime
        .update_task_with_identity(
            &task.id,
            TaskUpdateParams {
                complexity: Some(TaskComplexity::Hard),
                tags: Some(vec!["scope-expanded".into()]),
                ..Default::default()
            },
            None,
            None,
        )
        .unwrap();
    assert_eq!(updated.complexity, Some(TaskComplexity::Hard));
    assert_eq!(updated.tags, vec!["scope-expanded"]);
    assert_eq!(
        updated.crew, task.crew,
        "ORB-19929: admission owns the implementation crew"
    );
    assert_eq!(updated.crew_source, task.crew_source);
    assert_eq!(redraws(&workspace, &task), 0);
    let after = workspace
        .jobs
        .get_job_run(&admitted.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(after.input, before.input);
    assert_eq!(before.crew_model.as_deref(), Some("fixture-low"));
    assert_eq!(after.crew_model, before.crew_model);
    assert_eq!(after.resolved_crew, before.resolved_crew);
    let shown = orbit_core::application::job::job_run_to_json(&after, None);
    assert_eq!(shown["requested_crew"], "low_lane");

    // Pilot's shared rerating port must respect the same backlog reservation.
    assess(&workspace, &updated, TaskComplexity::Low);
    let reserved = workspace.runtime.get_task(&task.id).unwrap();
    assess(&workspace, &reserved, TaskComplexity::Hard);
    assert_eq!(
        workspace.runtime.get_task(&task.id).unwrap().crew,
        task.crew
    );
    assert_eq!(redraws(&workspace, &task), 0);
}

fn install_delivery_job(workspace: &Workspace) {
    let jobs = workspace.runtime.global_root().join("resources/jobs");
    std::fs::create_dir_all(&jobs).unwrap();
    std::fs::write(jobs.join("task_local_pipeline.yaml"),
        "schemaVersion: 2\nkind: Job\nmetadata:\n  name: task_local_pipeline\nspec:\n  state: enabled\n  steps: []\n").unwrap();
    orbit_core::test_support::install_substitute_pipeline_worker([
        std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        "--list".to_string(),
    ]);
}

#[test]
fn admitted_type_edits_keep_crew_until_fresh_admission_and_explicit_correction() {
    if !isolated(
        "task_pilot::crew_selection::admitted_scope::admitted_type_edits_keep_crew_until_fresh_admission_and_explicit_correction",
    ) {
        return;
    }
    use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
    use orbit_types::workflow::JobRunState;
    for status in [
        TaskStatus::Backlog,
        TaskStatus::InProgress,
        TaskStatus::Review,
    ] {
        let mut workspace = workspace();
        let config = workspace.repo.join(".orbit/config.toml");
        let text = std::fs::read_to_string(&config).unwrap();
        std::fs::write(config, format!("{text}\n[[workflow.crew_performance_types]]\nname = \"ui\"\ntags = [\"ui\"]\npaths = [\"ui/**\"]\n[workflow.type_complexity_crews.ui]\nhard = [\"low_lane\"]\n")).unwrap();
        workspace.runtime = runtime_at(
            &workspace.runtime.global_root(),
            &workspace.repo.join(".orbit"),
        );
        std::fs::create_dir_all(workspace.repo.join("ui")).unwrap();
        std::fs::write(workspace.repo.join("ui/view.mjs"), "export {};\n").unwrap();
        let task = task(&workspace, false);
        let task = workspace
            .runtime
            .update_task_with_identity(
                &task.id,
                TaskUpdateParams {
                    complexity: Some(TaskComplexity::Hard),
                    ..Default::default()
                },
                None,
                None,
            )
            .unwrap();
        assert_eq!(task.crew.as_deref(), Some("hard_lane"));
        // The identical scope change must still route an unadmitted task.
        let routed = workspace
            .runtime
            .update_task_with_identity(
                &task.id,
                TaskUpdateParams {
                    context_files: Some(vec!["file:ui/view.mjs".into()]),
                    ..Default::default()
                },
                None,
                None,
            )
            .unwrap();
        assert_eq!(routed.crew.as_deref(), Some("low_lane"));
        assert_eq!(routed.crew_source.as_deref(), Some("type:ui/hard"));
        let task = workspace
            .runtime
            .update_task_with_identity(
                &task.id,
                TaskUpdateParams {
                    context_files: Some(task.context_files),
                    ..Default::default()
                },
                None,
                None,
            )
            .unwrap();
        assert_eq!(task.crew.as_deref(), Some("hard_lane"));
        install_delivery_job(&workspace);
        let held_worker = hold_delivery_worker();
        let run = workspace
            .runtime
            .submit_pipeline_run(
                "task_local_pipeline",
                json!({"task_id": task.id}),
                None,
                Some("fixture"),
            )
            .unwrap();
        held_worker.wait_for_reap();
        workspace
            .runtime
            .apply_task_automation_update(
                &task.id,
                TaskAutomationUpdate {
                    status: Some(status),
                    job_run_id: Some(run.run_id.clone()),
                    ..Default::default()
                },
            )
            .unwrap();
        let redraws_before = redraws(&workspace, &task);
        for edit in [
            TaskUpdateParams {
                context_files: Some(vec!["file:ui/view.mjs".into()]),
                ..Default::default()
            },
            TaskUpdateParams {
                tags: Some(vec!["ui".into()]),
                ..Default::default()
            },
            TaskUpdateParams {
                tags: Some(vec!["ui".into()]),
                complexity: Some(TaskComplexity::Hard),
                ..Default::default()
            },
            TaskUpdateParams {
                complexity: Some(TaskComplexity::Low),
                ..Default::default()
            },
        ] {
            let updated = workspace
                .runtime
                .update_task_with_identity(&task.id, edit, None, Some("codex".into()))
                .unwrap();
            assert_eq!(
                updated.crew, task.crew,
                "ORB-19929: {status} scope edits preserve admitted crew"
            );
            assert_eq!(updated.crew_source, task.crew_source);
            assert_eq!(updated.context_files, vec!["file:ui/view.mjs"]);
        }
        assert_eq!(redraws(&workspace, &task), redraws_before);
        assert_eq!(
            workspace
                .runtime
                .list_tasks()
                .unwrap()
                .into_iter()
                .find(|row| row.id == task.id)
                .unwrap()
                .crew,
            task.crew
        );
        let historical = workspace
            .jobs
            .get_job_run(&run.run_id)
            .unwrap()
            .unwrap()
            .input;
        drop(held_worker);
        workspace
            .jobs
            .finalize_job_run(
                &run.run_id,
                JobRunState::Cancelled,
                chrono::Utc::now(),
                None,
            )
            .unwrap();
        // Internal recovery returns this isolated task for a genuinely fresh run.
        workspace
            .runtime
            .apply_task_automation_update(
                &task.id,
                TaskAutomationUpdate {
                    status: Some(TaskStatus::Backlog),
                    ..Default::default()
                },
            )
            .unwrap();
        let fresh = workspace
            .runtime
            .submit_pipeline_run(
                "task_local_pipeline",
                json!({"task_id": task.id}),
                None,
                Some("fixture"),
            )
            .unwrap();
        let fresh_input = workspace
            .runtime
            .show_job_run_observed(&fresh.run_id)
            .unwrap()
            .input
            .unwrap();
        assert_eq!(fresh_input["crew_selection"]["crew"], "low_lane");
        assert_eq!(
            workspace
                .runtime
                .get_task(&task.id)
                .unwrap()
                .crew
                .as_deref(),
            Some("low_lane")
        );
        let corrected = workspace
            .runtime
            .update_task_as_human(
                &task.id,
                TaskUpdateParams {
                    crew: Some(Some("hard_lane".into())),
                    ..Default::default()
                },
                "human:fixture".into(),
            )
            .unwrap();
        assert_eq!(corrected.crew_source.as_deref(), Some("explicit"));
        assert_eq!(
            workspace
                .runtime
                .show_job_run_observed(&run.run_id)
                .unwrap()
                .input,
            historical
        );
        assert!(
            workspace
                .runtime
                .get_task_history(&task.id)
                .unwrap()
                .iter()
                .any(|entry| entry.event == "crew_assigned" && entry.by.starts_with("human:"))
        );
    }
}

#[test]
fn admission_serializes_scope_edits_between_selection_and_insertion() {
    if !isolated(
        "task_pilot::crew_selection::admitted_scope::admission_serializes_scope_edits_between_selection_and_insertion",
    ) {
        return;
    }
    let workspace = workspace();
    let task = task(&workspace, false);
    install_delivery_job(&workspace);
    let registry = orbit_store::maintenance::task_registry::TaskRegistryStore::open(
        &orbit_store::maintenance::task_registry::task_registry_path(
            &workspace.runtime.global_root(),
        ),
    )
    .unwrap();
    let lock_target = registry
        .canonical_task_bundle_path(&workspace.runtime.workspace_id().unwrap(), &task.id)
        .unwrap()
        .with_extension("bundle");
    let held_worker = hold_delivery_worker();
    let admitted = orbit_core::test_support::with_crew_selection_checkpoint(
        move || {
            // Probe the same bundle write lock ordinary task updates take.
            // An independent thread cannot reenter the admitting thread's lock.
            let error = std::thread::spawn(move || {
                orbit_common::fs::io::hold_exclusive_file_lock_options(
                    &lock_target,
                    "scope-update fixture",
                    orbit_common::fs::io::FileLockOptions {
                        timeout: std::time::Duration::from_millis(20),
                        ..Default::default()
                    },
                )
                .map(|_| ())
            })
            .join()
            .unwrap()
            .expect_err("ORB-19929: selection must hold the scope-update lock through insertion");
            assert_eq!(error.kind(), std::io::ErrorKind::TimedOut, "{error}");
        },
        || {
            workspace.runtime.submit_pipeline_run(
                "task_local_pipeline",
                json!({"task_id": task.id}),
                None,
                Some("fixture"),
            )
        },
    )
    .unwrap();
    held_worker.wait_for_reap();
    let updated = workspace
        .runtime
        .update_task_with_identity(
            &task.id,
            TaskUpdateParams {
                complexity: Some(TaskComplexity::Hard),
                ..Default::default()
            },
            None,
            None,
        )
        .unwrap();
    let run = workspace
        .runtime
        .show_job_run_observed(&admitted.run_id)
        .unwrap();
    assert_eq!(
        run.input.unwrap()["crew_selection"]["crew"],
        updated.crew.unwrap()
    );
    assert_eq!(updated.complexity, Some(TaskComplexity::Hard));
    assert_eq!(redraws(&workspace, &task), 0);
}

/// Keep the real admitted run pending while its substitute worker is reaped.
/// Channels establish the boundary; dropping the guard always releases it.
struct HeldDeliveryWorker {
    release: std::sync::mpsc::Sender<()>,
    reaped: std::sync::mpsc::Receiver<()>,
}

impl HeldDeliveryWorker {
    fn wait_for_reap(&self) {
        self.reaped
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
    }
}

impl Drop for HeldDeliveryWorker {
    fn drop(&mut self) {
        let _ = self.release.send(());
    }
}

fn hold_delivery_worker() -> HeldDeliveryWorker {
    let (release, wait) = std::sync::mpsc::channel();
    let (notify, reaped) = std::sync::mpsc::channel();
    orbit_core::test_support::install_worker_reap_hook(
        orbit_core::test_support::WorkerReapPhase::BeforeExitRecorded,
        move |_| {
            notify.send(()).unwrap();
            let _ = wait.recv();
        },
    );
    HeldDeliveryWorker { release, reaped }
}
