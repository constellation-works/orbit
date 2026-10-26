//! Large valid arm weights keep the public sample-ratio readout correct.

use chrono::Utc;
use orbit_core::application::experiment::{
    ExperimentActor, ExperimentChange, ExperimentDefinitionInput,
};
use orbit_core::application::task::TaskAddParams;
use orbit_core::{OrbitRuntime, TaskComplexity, TaskStatus, TaskType};
use orbit_store::contracts::{ExperimentAssignment, ExperimentStoreBackend};
use serde_json::json;

#[test]
fn large_valid_arm_weights_produce_the_expected_sample_ratio() {
    if !super::dispatch_admission::isolated(
        "experiment_readout::large_valid_arm_weights_produce_the_expected_sample_ratio",
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
        actor: "readout-test".into(),
        reason: None,
    };
    let experiment = runtime
        .create_experiment(
            "sample-ratio-overflow",
            ExperimentDefinitionInput {
                arms: Some(json!([
                    { "name": "control", "control": true, "weight": 3_000_000_000_u64 },
                    { "name": "treatment", "weight": 3_000_000_000_u64 }
                ])),
                primary_metric: Some("implement_tokens".into()),
                ..ExperimentDefinitionInput::default()
            },
            actor.clone(),
        )
        .unwrap();
    let experiment_id = experiment.id;
    let started = runtime
        .change_experiment(&experiment_id, ExperimentChange::Start, actor)
        .unwrap();
    assert_eq!(started.segment, 1);

    let workspace_id = runtime.workspace_id().unwrap();
    let store = runtime.sqlite_store().unwrap();
    for index in 0..200 {
        let task = runtime
            .add_task(TaskAddParams {
                title: format!("sample ratio task {index}"),
                complexity: TaskComplexity::Low,
                task_type: Some(TaskType::Chore),
                status: Some(TaskStatus::Backlog),
                ..TaskAddParams::default()
            })
            .unwrap();
        let task_id = task.id.to_string();
        let arm_name = if index < 100 { "control" } else { "treatment" };
        let assignments = store
            .assign_experiments(&workspace_id, &task_id, true, &|records, _, _| {
                let record = records
                    .iter()
                    .find(|record| record.experiment.id == experiment_id)
                    .expect("started experiment is in the store");
                let arm = record
                    .arms
                    .iter()
                    .find(|arm| arm.name == arm_name)
                    .expect("assigned arm is in the experiment");
                Ok(vec![ExperimentAssignment {
                    experiment: experiment_id.clone(),
                    task_id: task_id.clone(),
                    arm: Some(arm_name.into()),
                    segment: Some(1),
                    digest: Some(arm.overlay_digest.clone()),
                    assigned_at: Utc::now(),
                    forced: false,
                    exclusion_reason: None,
                }])
            })
            .unwrap();
        assert_eq!(assignments.len(), 1);
    }

    let readout = runtime.experiment_readout(&experiment_id, None).unwrap();
    assert_eq!(readout.enrollment.analysed, 200);
    let sample_ratio = &readout.sample_ratio;
    let segment = sample_ratio.segments.first().unwrap();
    assert_eq!(segment.observed["control"], 100);
    assert_eq!(segment.observed["treatment"], 100);
    assert_eq!(segment.expected["control"], 100.0);
    assert_eq!(segment.expected["treatment"], 100.0);
    assert_eq!(sample_ratio.chi_square, Some(0.0));
    assert_eq!(sample_ratio.p_value, Some(1.0));
    assert!(!sample_ratio.mismatch);
}
