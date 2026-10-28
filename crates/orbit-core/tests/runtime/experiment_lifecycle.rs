//! Experiment reweight events distinguish weight edits from arm retirement.

use orbit_core::OrbitRuntime;
use orbit_core::application::experiment::{
    ExperimentActor, ExperimentChange, ExperimentDefinitionInput,
};
use orbit_core::application::task::TaskAddParams;
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
    let legacy_task = stale_runtime
        .add_task(TaskAddParams {
            title: "Task with a legacy admission-only row".into(),
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
                primary_metric: Some("implement_tokens".into()),
                ..ExperimentDefinitionInput::default()
            },
            actor.clone(),
        )
        .unwrap();
    // Reproduce a historical admission-only row while enabled but before any
    // experiment was running. Its capture is empty, so no assignment is sticky.
    let legacy_capture = submit_experiment_capture(&enabled_runtime, &job_path, &legacy_task.id);
    assert!(legacy_capture["assignments"].as_array().unwrap().is_empty());
    enabled_runtime
        .change_experiment(&experiment.id, ExperimentChange::Start, actor)
        .unwrap();

    // Both submissions use the runtime created before enabling. It still has
    // the old disabled settings snapshot, yet the stored running experiment
    // enrolls eligible tasks and recovers the legacy admission-only row.
    let recovered_disabled =
        submit_experiment_capture(&stale_runtime, &job_path, &disabled_task.id);
    assert_eq!(
        recovered_disabled["assignments"].as_array().unwrap().len(),
        1
    );
    let recovered_legacy = submit_experiment_capture(&stale_runtime, &job_path, &legacy_task.id);
    assert_eq!(recovered_legacy["assignments"].as_array().unwrap().len(), 1);
    // The assigned arm is drawn from the experiment's random salt, so pin the
    // persisted arm to the capture's arm rather than to a specific arm.
    let captured_arm = recovered_legacy["assignments"][0]["arm"].as_str();
    assert!(matches!(captured_arm, Some("control" | "treatment")));
    assert_eq!(
        stale_runtime.task_experiments(&legacy_task.id).unwrap()[0]
            .arm
            .as_deref(),
        captured_arm
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
                primary_metric: Some("implement_tokens".into()),
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
    let experiment = runtime
        .create_experiment(
            name,
            ExperimentDefinitionInput {
                arms: Some(json!([
                    { "name": "control", "control": true, "weight": 1 },
                    { "name": "treatment-a", "weight": 1 },
                    { "name": "treatment-b", "weight": 1 }
                ])),
                primary_metric: Some("implement_tokens".into()),
                ..ExperimentDefinitionInput::default()
            },
            actor.clone(),
        )
        .unwrap();
    runtime
        .change_experiment(&experiment.id, ExperimentChange::Start, actor)
        .unwrap();
    experiment.id
}
