use super::*;
use orbit_types::workflow::{
    CrewMemoryBlock, CrewMemoryDelivery, CrewMemoryRevisions, TagInstructionRole,
};

/// A shadow memory arm captures but delivers nothing: the readout must
/// still report the captured digest and (null) revisions, labelled
/// captured, from the run's `experiment_capture`.
#[test]
fn shadow_memory_arm_reports_captured_digest_with_null_revisions() {
    if !crate::dispatch_admission::isolated(
        "experiment_readout::shadow_memory::shadow_memory_arm_reports_captured_digest_with_null_revisions",
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
        "[experiments]\nenabled = true\n[crew_memory]\nenabled = true\n",
    )
    .unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let actor = ExperimentActor {
        actor: "readout-test".into(),
        reason: None,
    };
    let experiment = runtime
        .create_experiment(
            "shadow-memory-capture",
            ExperimentDefinitionInput {
                mode: Some("shadow".into()),
                arms: Some(json!([
                    { "name": "control", "control": true },
                    { "name": "memory", "overlay": { "memory": { "roles": ["implement"], "global": true } } }
                ])),
                primary_metric: Some("first_pass_acceptance".into()),
                ..ExperimentDefinitionInput::default()
            },
            actor.clone(),
        )
        .unwrap();
    runtime
        .change_experiment(&experiment.id, ExperimentChange::Start, actor)
        .unwrap();

    let task_id = enroll_arm_task(&runtime, &experiment.id, "memory", TaskComplexity::Low);
    let control_id = enroll_arm_task(&runtime, &experiment.id, "control", TaskComplexity::Low);

    // The owner captured an empty block for the drawn model: both
    // revisions are null, yet a digest and renderer version exist.
    let delivery = CrewMemoryDelivery::render(
        TagInstructionRole::Implement,
        "model-null-rev",
        true,
        Some("2026-11-02T02:00:00Z".into()),
        CrewMemoryBlock {
            model: Vec::new(),
            global: String::new(),
            revision: CrewMemoryRevisions {
                model: None,
                global: None,
            },
        },
    );
    let expected_digest = delivery.digest.clone();
    let overlay_json = json!({"memory":{"roles":["implement"],"global":true}}).to_string();
    // The assignment digest must match the stored arm digest; reuse the
    // capture shape the owner writes into a run input.
    let store = runtime.sqlite_store().unwrap();
    let record = store
        .experiment_record(&runtime.workspace_id().unwrap(), &experiment.id)
        .unwrap()
        .unwrap();
    let arm = record.arms.iter().find(|arm| arm.name == "memory").unwrap();
    let capture = json!({
        "task_id": task_id,
        "assignments": [{
            "experiment": experiment.id,
            "arm": "memory",
            "segment": 1,
            "forced": false,
            "digest": arm.overlay_digest,
            "overlay_json": overlay_json,
            "live": false,
            "memory": { "enabled": true, "deliveries": [delivery] }
        }]
    });
    runtime
        .stores()
        .jobs()
        .insert_job_run(
            "task_local_pipeline",
            1,
            Utc::now(),
            Some(
                json!({ "task_id": task_id, "task_ids": [task_id], "experiment_capture": capture }),
            ),
            None,
        )
        .unwrap();

    let readout = runtime.experiment_readout(&experiment.id, None).unwrap();
    assert_eq!(readout.experiment.mode, "shadow");
    let arm = readout
        .arms
        .iter()
        .find(|arm| arm.name == "memory")
        .unwrap();
    assert_eq!(arm.task_ids, vec![task_id.clone()]);
    assert!(
        arm.memory_strata.is_empty(),
        "shadow delivers nothing, so delivered strata stay empty"
    );
    assert_eq!(arm.memory_captures.len(), 1);
    let capture = &arm.memory_captures[0];
    assert_eq!(capture.task_id, task_id);
    assert_eq!(capture.digest, expected_digest);
    assert_eq!(capture.model_revision, None);
    assert_eq!(capture.global_revision, None);
    assert_eq!(capture.renderer_version, 1);
    assert_eq!(arm.memory_capture_strata.len(), 1);
    assert_eq!(arm.memory_capture_strata[0].tasks, 1);
    assert_eq!(
        arm.memory_capture_strata[0].digests,
        vec![expected_digest.clone()]
    );

    let json = serde_json::to_value(&readout).unwrap();
    let memory_arm = json["arms"]
        .as_array()
        .unwrap()
        .iter()
        .find(|arm| arm["name"] == "memory")
        .unwrap();
    assert_eq!(memory_arm["memory_captures"][0]["digest"], expected_digest);
    assert!(memory_arm["memory_captures"][0]["model_revision"].is_null());
    assert!(memory_arm["memory_captures"][0]["global_revision"].is_null());
    assert_eq!(memory_arm["memory_captures"][0]["renderer_version"], 1);
    assert_eq!(memory_arm["task_ids"][0], task_id);
    let control_arm = json["arms"]
        .as_array()
        .unwrap()
        .iter()
        .find(|arm| arm["name"] == "control")
        .unwrap();
    assert_eq!(control_arm["task_ids"][0], control_id);
    assert!(control_arm.get("memory_captures").is_none());
}
