//! Experiment reweight events distinguish weight edits from arm retirement.

use orbit_common::OrbitError;
use orbit_core::OrbitRuntime;
use orbit_core::application::experiment::{
    ExperimentActor, ExperimentChange, ExperimentDefinitionInput,
};
use orbit_core::application::task::{TaskAddParams, TaskUpdateParams};
use orbit_core::{TaskComplexity, TaskStatus, TaskType};
use serde_json::json;

#[test]
fn disabled_and_stale_admissions_enroll_once_experiments_run() {
    if !super::dispatch_admission::isolated(
        "experiment_lifecycle::disabled_and_stale_admissions_enroll_once_experiments_run",
    ) {
        return;
    }

    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("home/.orbit");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let config_path = workspace.join("config.toml");
    std::fs::write(
        &config_path,
        "[workflow]\ndefault_crew = \"sol\"\n\n[crews.sol]\nprovider = \"codex\"\nmodel = \"test\"\n\n[experiments]\nenabled = false\n",
    )
    .unwrap();
    let job_path = capture_job(root.path());

    // This runtime keeps the disabled settings snapshot after the config is
    // enabled below, just like a long-lived drain process.
    let stale_runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let disabled_task = stale_runtime
        .add_task(TaskAddParams {
            title: "Task admitted while experiments are disabled".into(),
            complexity: TaskComplexity::Medium,
            task_type: Some(TaskType::Chore),
            status: Some(TaskStatus::Backlog),
            ..TaskAddParams::default()
        })
        .unwrap();

    let disabled_capture = submit_experiment_capture(&stale_runtime, &job_path, &disabled_task.id);
    assert!(
        disabled_capture["assignments"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    std::fs::write(
        &config_path,
        "[workflow]\ndefault_crew = \"sol\"\n\n[crews.sol]\nprovider = \"codex\"\nmodel = \"test\"\n\n[experiments]\nenabled = true\n",
    )
    .unwrap();
    let enabled_runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let actor = ExperimentActor {
        actor: "experiment-lifecycle-test".into(),
        reason: None,
    };
    let experiment = enabled_runtime
        .create_experiment(
            "admission-recovery",
            ExperimentDefinitionInput {
                arms: Some(json!([
                    { "name": "control", "control": true },
                    { "name": "treatment" }
                ])),
                primary_metric: Some("total_tokens".into()),
                ..ExperimentDefinitionInput::default()
            },
            actor.clone(),
        )
        .unwrap();
    enabled_runtime
        .change_experiment(&experiment.id, ExperimentChange::Start, actor)
        .unwrap();

    // The submission uses the runtime created before enabling. It still has
    // the old disabled settings snapshot, yet the disabled admission wrote
    // nothing, so this is the task's first admission and it enrolls.
    let recovered_disabled =
        submit_experiment_capture(&stale_runtime, &job_path, &disabled_task.id);
    assert_eq!(
        recovered_disabled["assignments"].as_array().unwrap().len(),
        1
    );
    // The assigned arm is drawn from the experiment's random salt, so pin the
    // persisted arm to the capture's arm rather than to a specific arm.
    let captured_arm = recovered_disabled["assignments"][0]["arm"].as_str();
    assert!(matches!(captured_arm, Some("control" | "treatment")));
    assert_eq!(
        stale_runtime.task_experiments(&disabled_task.id).unwrap()[0]
            .arm
            .as_deref(),
        captured_arm
    );
}

/// [ORB-19423] An enabled first admission that misses every running
/// population is final: tagging the task into a population and retrying must
/// not enroll it, while assignments and surface-conflict exclusions stay put.
#[test]
fn enabled_population_miss_stays_final_across_retries() {
    if !super::dispatch_admission::isolated(
        "experiment_lifecycle::enabled_population_miss_stays_final_across_retries",
    ) {
        return;
    }

    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("home/.orbit");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(
        workspace.join("config.toml"),
        "[workflow]\ndefault_crew = \"sol\"\n\n[crews.sol]\nprovider = \"codex\"\nmodel = \"test\"\n\n[experiments]\nenabled = true\n",
    )
    .unwrap();
    let job_path = capture_job(root.path());
    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let actor = ExperimentActor {
        actor: "experiment-lifecycle-test".into(),
        reason: None,
    };
    let start = |name: &str, tag: &str, overlay: serde_json::Value| {
        let experiment = runtime
            .create_experiment(
                name,
                ExperimentDefinitionInput {
                    arms: Some(json!([
                        { "name": "control", "control": true, "overlay": overlay },
                        { "name": "treatment", "overlay": overlay }
                    ])),
                    population: Some(json!({ "tags_include": [tag] })),
                    primary_metric: Some("total_tokens".into()),
                    ..ExperimentDefinitionInput::default()
                },
                actor.clone(),
            )
            .unwrap();
        runtime
            .change_experiment(&experiment.id, ExperimentChange::Start, actor.clone())
            .unwrap();
        experiment.id
    };
    let tagged = start("tagged", "eligible", json!({}));
    let crew_overlay = json!({ "crew": { "pool": [{ "crew": "sol", "weight": 1 }] } });
    let first_crew = start("crew-first", "contested", crew_overlay.clone());
    let second_crew = start("crew-second", "contested", crew_overlay);
    let add = |title: &str, tags: &[&str]| {
        runtime
            .add_task(TaskAddParams {
                title: title.into(),
                complexity: TaskComplexity::Medium,
                task_type: Some(TaskType::Chore),
                status: Some(TaskStatus::Backlog),
                tags: tags.iter().map(|tag| tag.to_string()).collect(),
                ..TaskAddParams::default()
            })
            .unwrap()
            .id
    };

    // A task carrying the tag enrolls, so the filter is the only difference.
    let matched = add("Task in the tagged population", &["eligible"]);
    let matched_capture = submit_experiment_capture(&runtime, &job_path, &matched);
    assert_eq!(matched_capture["assignments"][0]["experiment"], tagged);

    let missed = add("Task outside every population", &[]);
    let missed_capture = submit_experiment_capture(&runtime, &job_path, &missed);
    assert!(missed_capture["assignments"].as_array().unwrap().is_empty());
    runtime
        .update_task_with_identity(
            &missed,
            TaskUpdateParams {
                tags: Some(vec!["eligible".into()]),
                ..TaskUpdateParams::default()
            },
            None,
            None,
        )
        .unwrap();
    let retried = submit_experiment_capture(&runtime, &job_path, &missed);
    assert!(
        retried["assignments"].as_array().unwrap().is_empty(),
        "a retry after the edit enrolled the task: {retried}"
    );
    assert!(runtime.task_experiments(&missed).unwrap().is_empty());

    let contested = add("Task in two crew experiments", &["contested"]);
    submit_experiment_capture(&runtime, &job_path, &contested);
    let rows = runtime.task_experiments(&contested).unwrap();
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[0].experiment, first_crew);
    assert!(rows[0].arm.is_some(), "{rows:?}");
    assert_eq!(rows[1].experiment, second_crew);
    assert_eq!(
        rows[1].exclusion_reason.as_deref(),
        Some("surface_conflict")
    );
    submit_experiment_capture(&runtime, &job_path, &contested);
    let retried_rows = runtime.task_experiments(&contested).unwrap();
    assert_eq!(
        (
            retried_rows[0].arm.as_deref(),
            retried_rows[1].exclusion_reason.as_deref()
        ),
        (rows[0].arm.as_deref(), Some("surface_conflict"))
    );
}

#[test]
fn disabled_setting_stops_new_enrollment_but_keeps_sticky_rows() {
    if !super::dispatch_admission::isolated(
        "experiment_lifecycle::disabled_setting_stops_new_enrollment_but_keeps_sticky_rows",
    ) {
        return;
    }

    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("home/.orbit");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let config_path = workspace.join("config.toml");
    std::fs::write(
        &config_path,
        "[workflow]\ndefault_crew = \"sol\"\n\n[crews.sol]\nprovider = \"codex\"\nmodel = \"test\"\n\n[experiments]\nenabled = true\n",
    )
    .unwrap();
    let job_path = capture_job(root.path());

    let enabled_runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let actor = ExperimentActor {
        actor: "experiment-lifecycle-test".into(),
        reason: None,
    };
    let experiment = enabled_runtime
        .create_experiment(
            "disabled-after-start",
            ExperimentDefinitionInput {
                arms: Some(json!([
                    { "name": "control", "control": true },
                    { "name": "treatment" }
                ])),
                primary_metric: Some("total_tokens".into()),
                ..ExperimentDefinitionInput::default()
            },
            actor.clone(),
        )
        .unwrap();
    enabled_runtime
        .change_experiment(&experiment.id, ExperimentChange::Start, actor)
        .unwrap();
    let sticky_task = enabled_runtime
        .add_task(TaskAddParams {
            title: "Task enrolled while the experiment runs".into(),
            complexity: TaskComplexity::Medium,
            task_type: Some(TaskType::Chore),
            status: Some(TaskStatus::Backlog),
            ..TaskAddParams::default()
        })
        .unwrap();
    let fresh_task = enabled_runtime
        .add_task(TaskAddParams {
            title: "Task admitted after experiments were disabled".into(),
            complexity: TaskComplexity::Medium,
            task_type: Some(TaskType::Chore),
            status: Some(TaskStatus::Backlog),
            ..TaskAddParams::default()
        })
        .unwrap();

    let sticky_capture = submit_experiment_capture(&enabled_runtime, &job_path, &sticky_task.id);
    assert_eq!(sticky_capture["assignments"].as_array().unwrap().len(), 1);
    let sticky_rows = enabled_runtime.task_experiments(&sticky_task.id).unwrap();

    // Turn experiments off without pausing the running experiment, then open a
    // fresh runtime that reads the disabled setting.
    std::fs::write(
        &config_path,
        "[workflow]\ndefault_crew = \"sol\"\n\n[crews.sol]\nprovider = \"codex\"\nmodel = \"test\"\n\n[experiments]\nenabled = false\n",
    )
    .unwrap();
    let disabled_runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();

    let fresh_capture = submit_experiment_capture(&disabled_runtime, &job_path, &fresh_task.id);
    assert!(fresh_capture["assignments"].as_array().unwrap().is_empty());
    assert!(
        disabled_runtime
            .task_experiments(&fresh_task.id)
            .unwrap()
            .is_empty()
    );

    // The sticky assignment is still captured and stored, unchanged.
    let kept_capture = submit_experiment_capture(&disabled_runtime, &job_path, &sticky_task.id);
    assert_eq!(kept_capture["assignments"], sticky_capture["assignments"]);
    assert_eq!(
        disabled_runtime.task_experiments(&sticky_task.id).unwrap(),
        sticky_rows
    );
}

#[test]
fn start_refuses_after_experiments_are_disabled_on_an_open_runtime() {
    if !super::dispatch_admission::isolated(
        "experiment_lifecycle::start_refuses_after_experiments_are_disabled_on_an_open_runtime",
    ) {
        return;
    }

    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("home/.orbit");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let config_path = workspace.join("config.toml");
    std::fs::write(&config_path, "[experiments]\nenabled = true\n").unwrap();
    let actor = ExperimentActor {
        actor: "experiment-lifecycle-test".into(),
        reason: None,
    };

    // This runtime keeps the enabled settings snapshot after the operator
    // disables experiments, just like a long-lived drain process.
    let stale_runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let experiment_id = draft_experiment(&stale_runtime, "disabled-before-start", actor.clone());

    std::fs::write(&config_path, "[experiments]\nenabled = false\n").unwrap();

    let error = stale_runtime
        .change_experiment(&experiment_id, ExperimentChange::Start, actor)
        .unwrap_err();
    assert!(
        matches!(&error, OrbitError::PolicyDenied(message) if message.contains("experiments.enabled")),
        "start must refuse on the current config, got {error:?}"
    );
    assert_eq!(
        stale_runtime
            .experiment_view(&experiment_id)
            .unwrap()
            .status,
        "draft"
    );
}

#[test]
fn start_accepts_after_experiments_are_enabled_on_an_open_runtime() {
    if !super::dispatch_admission::isolated(
        "experiment_lifecycle::start_accepts_after_experiments_are_enabled_on_an_open_runtime",
    ) {
        return;
    }

    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("home/.orbit");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let config_path = workspace.join("config.toml");
    std::fs::write(&config_path, "[experiments]\nenabled = false\n").unwrap();
    let actor = ExperimentActor {
        actor: "experiment-lifecycle-test".into(),
        reason: None,
    };

    // This runtime keeps the disabled settings snapshot after the operator
    // enables experiments, so start must read the current config as admission does.
    let stale_runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let experiment_id = draft_experiment(&stale_runtime, "enabled-before-start", actor.clone());

    std::fs::write(&config_path, "[experiments]\nenabled = true\n").unwrap();

    let started = stale_runtime
        .change_experiment(&experiment_id, ExperimentChange::Start, actor)
        .unwrap();
    assert_eq!(started.status, "running");
}

/// [ORB-20670] A legacy draft whose stored primary metric is step-local is
/// refused at start with the whole-task alternatives, before any enrollment;
/// a draft with a whole-task primary still starts.
#[test]
fn start_refuses_legacy_step_local_primary() {
    if !super::dispatch_admission::isolated(
        "experiment_lifecycle::start_refuses_legacy_step_local_primary",
    ) {
        return;
    }

    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("home/.orbit");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(
        workspace.join("config.toml"),
        "[workflow]\ndefault_crew = \"sol\"\n\n[crews.sol]\nprovider = \"codex\"\nmodel = \"test\"\n\n[experiments]\nenabled = true\n",
    )
    .unwrap();
    let job_path = capture_job(root.path());
    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let actor = ExperimentActor {
        actor: "experiment-lifecycle-test".into(),
        reason: None,
    };

    // A draft stored before step-local primaries were refused on write.
    let legacy_id = draft_experiment(&runtime, "legacy-step-local", actor.clone());
    runtime
        .sqlite_store()
        .unwrap()
        .connection()
        .lock()
        .unwrap()
        .execute(
            "UPDATE experiments SET primary_metric = 'implement_tokens' WHERE id = ?1",
            [&legacy_id],
        )
        .unwrap();

    let error = runtime
        .change_experiment(&legacy_id, ExperimentChange::Start, actor.clone())
        .unwrap_err();
    assert!(
        matches!(&error, OrbitError::InvalidInput(message) if message.contains("implement_tokens") && message.contains("step-local") && message.contains("choose a whole-task metric") && message.contains("total_tokens")),
        "start must refuse a step-local stored primary with the whole-task alternatives, got {error:?}"
    );
    let legacy = runtime.experiment_view(&legacy_id).unwrap();
    assert_eq!(legacy.status, "draft");
    assert!(
        legacy.events.iter().all(|event| event.kind != "started"),
        "a refused start must not record a start event: {:?}",
        legacy.events
    );

    // The refused start enrolls nothing.
    let task = runtime
        .add_task(TaskAddParams {
            title: "Task admitted after a refused legacy start".into(),
            complexity: TaskComplexity::Medium,
            task_type: Some(TaskType::Chore),
            status: Some(TaskStatus::Backlog),
            ..TaskAddParams::default()
        })
        .unwrap();
    let capture = submit_experiment_capture(&runtime, &job_path, &task.id);
    assert!(
        capture["assignments"].as_array().unwrap().is_empty(),
        "a refused start enrolled the task: {capture}"
    );
    assert!(runtime.task_experiments(&task.id).unwrap().is_empty());

    // A draft with a whole-task primary still starts.
    let started = runtime
        .change_experiment(
            &draft_experiment(&runtime, "whole-task-primary", actor.clone()),
            ExperimentChange::Start,
            actor,
        )
        .unwrap();
    assert_eq!(started.status, "running");
}

fn capture_job(root: &std::path::Path) -> std::path::PathBuf {
    let job_path = root.join("task_local_pipeline.yaml");
    std::fs::write(
        &job_path,
        "schemaVersion: 2\nkind: Job\nmetadata:\n  name: task_local_pipeline\nspec:\n  state: enabled\n  steps: []\n",
    )
    .unwrap();
    orbit_core::test_support::install_substitute_pipeline_worker([
        std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
        "--list".to_string(),
    ]);
    job_path
}

fn submit_experiment_capture(
    runtime: &OrbitRuntime,
    job_path: &std::path::Path,
    task_id: &str,
) -> serde_json::Value {
    let submitted = runtime
        .submit_job_run(
            job_path.to_str().unwrap(),
            json!({ "task_id": task_id, "crew": "sol" }),
            Some("experiment-lifecycle-test"),
        )
        .unwrap();
    let run = runtime.show_job_run_observed(&submitted.run_id).unwrap();
    run.input.unwrap()["experiment_capture"].clone()
}

#[test]
fn reweight_events_classify_weight_edits_separately_from_retirements() {
    if !super::dispatch_admission::isolated(
        "experiment_lifecycle::reweight_events_classify_weight_edits_separately_from_retirements",
    ) {
        return;
    }

    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("home/.orbit");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(
        workspace.join("config.toml"),
        "[experiments]\nenabled = true\n",
    )
    .unwrap();

    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let actor = ExperimentActor {
        actor: "experiment-lifecycle-test".into(),
        reason: None,
    };

    let retire_only_id = started_experiment(&runtime, "retire-only", actor.clone());
    runtime
        .change_experiment(
            &retire_only_id,
            ExperimentChange::Reweight {
                weights: None,
                retire: vec!["treatment-a".into()],
            },
            actor.clone(),
        )
        .unwrap();
    let retire_only = runtime
        .experiment_view(&retire_only_id)
        .unwrap()
        .events
        .pop()
        .unwrap();
    assert_eq!(retire_only.kind, "arm_retired");
    assert_eq!(retire_only.detail["weights_changed"], false);
    assert_eq!(retire_only.detail["retired"], json!(["treatment-a"]));

    let weight_only_id = started_experiment(&runtime, "weight-only", actor.clone());
    runtime
        .change_experiment(
            &weight_only_id,
            ExperimentChange::Reweight {
                weights: Some(json!({"treatment-a": 3})),
                retire: vec![],
            },
            actor.clone(),
        )
        .unwrap();
    let weight_only = runtime
        .experiment_view(&weight_only_id)
        .unwrap()
        .events
        .pop()
        .unwrap();
    assert_eq!(weight_only.kind, "reweighted");
    assert_eq!(weight_only.detail["weights_changed"], true);
    assert_eq!(weight_only.detail["retired"], json!([]));

    let combined_id = started_experiment(&runtime, "combined", actor.clone());
    runtime
        .change_experiment(
            &combined_id,
            ExperimentChange::Reweight {
                weights: Some(json!({"treatment-b": 4})),
                retire: vec!["treatment-a".into()],
            },
            actor,
        )
        .unwrap();
    let combined = runtime
        .experiment_view(&combined_id)
        .unwrap()
        .events
        .pop()
        .unwrap();
    assert_eq!(combined.kind, "reweighted");
    assert_eq!(combined.detail["weights_changed"], true);
    assert_eq!(combined.detail["retired"], json!(["treatment-a"]));
    assert_eq!(combined.detail["weights_after"]["treatment-b"], 4);
}

fn started_experiment(runtime: &OrbitRuntime, name: &str, actor: ExperimentActor) -> String {
    let experiment_id = draft_experiment(runtime, name, actor.clone());
    runtime
        .change_experiment(&experiment_id, ExperimentChange::Start, actor)
        .unwrap();
    experiment_id
}

fn draft_experiment(runtime: &OrbitRuntime, name: &str, actor: ExperimentActor) -> String {
    runtime
        .create_experiment(
            name,
            ExperimentDefinitionInput {
                arms: Some(json!([
                    { "name": "control", "control": true, "weight": 1 },
                    { "name": "treatment-a", "weight": 1 },
                    { "name": "treatment-b", "weight": 1 }
                ])),
                primary_metric: Some("total_tokens".into()),
                ..ExperimentDefinitionInput::default()
            },
            actor,
        )
        .unwrap()
        .id
}
