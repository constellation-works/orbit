//! Experiment reweight events distinguish weight edits from arm retirement.

use orbit_core::OrbitRuntime;
use orbit_core::application::experiment::{
    ExperimentActor, ExperimentChange, ExperimentDefinitionInput,
};
use serde_json::json;

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
