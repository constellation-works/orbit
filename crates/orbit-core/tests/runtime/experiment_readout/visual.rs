use super::*;

/// An unavailable post-landing comparison is missing data, not a measured
/// clean result in either experiment arm.
#[test]
fn unavailable_visual_measurement_is_not_zero_mismatches() {
    if !crate::dispatch_admission::isolated(
        "experiment_readout::visual::unavailable_visual_measurement_is_not_zero_mismatches",
    ) {
        return;
    }
    use orbit_engine::{RuntimeHost, TaskAutomationUpdate};
    use orbit_store::contracts::TaskArtifactUpdateParams;
    use orbit_store::maintenance::task_registry::{TaskRegistryStore, task_registry_path};
    use orbit_types::task::{ArtifactWriter, TaskArtifact};
    use orbit_types::workflow::{JobRunState, PipelineState};
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("global");
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
            "visual-unavailable",
            ExperimentDefinitionInput {
                arms: Some(json!([{"name":"control","control":true},{"name":"treatment"}])),
                primary_metric: Some("visual_landed_mismatches".into()),
                ..Default::default()
            },
            actor.clone(),
        )
        .unwrap();
    runtime
        .change_experiment(&experiment.id, ExperimentChange::Start, actor)
        .unwrap();
    let registry = TaskRegistryStore::open(&task_registry_path(&runtime.global_root())).unwrap();
    let stores = orbit_store::compose::workspace_coordinated_backends(
        registry,
        runtime.workspace_id().unwrap(),
        runtime.sqlite_store().unwrap(),
    )
    .unwrap()
    .task;
    for arm in ["control", "treatment"] {
        for case in ["unavailable", "incomplete", "stale", "clean"] {
            let task = enroll_arm_task(&runtime, &experiment.id, arm, TaskComplexity::Low);
            let jobs = orbit_store::compose::workspace_job_run_store(
                runtime.sqlite_store().unwrap(),
                runtime.workspace_id().unwrap(),
            );
            let run = jobs
                .insert_job_run(
                    "task_pr_pipeline",
                    1,
                    Utc::now(),
                    Some(json!({"task_ids":[task]})),
                    None,
                )
                .unwrap();
            let landed = "a".repeat(40);
            let measured = if case == "stale" {
                "b".repeat(40)
            } else {
                landed.clone()
            };
            let mut state =
                PipelineState::new(run.run_id.clone(), run.job_id, json!({"task_ids":[task]}));
            let output = json!({"phase":"complete","merge":{"merged":true,"landed_commit":landed},"completed_task_ids":[task]});
            state.record_step(
                0,
                JobRunState::Success,
                Some(output.clone()),
                Some(json!({"complete_pr":output})),
            );
            runtime.write_run_state(&run.run_id, &state).unwrap();
            for status in [TaskStatus::Review, TaskStatus::Done] {
                runtime
                    .apply_task_automation_update(
                        &task,
                        TaskAutomationUpdate {
                            status: Some(status),
                            job_run_id: Some(run.run_id.clone()),
                            ..Default::default()
                        },
                    )
                    .unwrap();
            }
            let mut record = json!({"schema_version":1,"purpose":"visual_measure","mode":"shadow",
            "task_id":task,"candidate_sha":"a".repeat(40),"outcome":"not_established",
            "recorded_only":true,"not_established":[{"reason":"browser_kit_missing","detail":"missing"}],
            "budget":{"max_mocks":1,"max_images":2,"max_image_tokens":1000,"max_findings":1,
                "images_shown":0,"estimated_image_tokens":0},
            "invalid_evidence_dropped":0,"over_max_findings_dropped":0,"findings":[]});
            record["candidate_sha"] = json!(measured);
            if case != "unavailable" {
                record["outcome"] = json!("reviewed");
                record["not_established"] = json!([]);
                record["budget"]["shown"] = json!([{"mock":"mocks/view.png","form":"pair","images":2,"estimated_image_tokens":1000}]);
            }
            let capture = json!({"schema_version":1,"mode":"shadow","outcome":"captured", "task_id":task,
            "host":"fixture","candidate_sha":measured,"manifests":[],"mocks":[{
            "artifact":"mocks/view.png","sha256":"0".repeat(64),"view":"view","state":"default","theme":"light","width":1440,
            "capture":{"path":"visual/measure/view/capture.png","side_by_side":"visual/measure/view/side-by-side.png",
            "side_by_side_sha256":"0".repeat(64),"commit":measured,"viewport":[1440,900],"device_scale_factor":1,
            "fixture":"default","fixture_sha256":"0".repeat(64),"captured_at":"2026-10-30T00:00:00Z",
            "sha256":"0".repeat(64),"stability":"stable","encoding":"png","lossy":false}}]});
            let mut capture = capture;
            if case == "incomplete" {
                capture["mocks"][0]["capture"] = serde_json::Value::Null;
            }
            let settle = json!({"schema_version":1,"candidate_sha":measured,"outcome":"established","refusals":[],"not_established":[],"held":false});
            stores
                .artifact
                .upsert_task_artifacts(
                    &task,
                    TaskArtifactUpdateParams {
                        actor: "system:fixture".into(),
                        writer: Some(ArtifactWriter::System),
                        upsert_artifacts: vec![
                            TaskArtifact::from_text(
                                "visual/measure/review.json",
                                record.to_string(),
                            ),
                            TaskArtifact::from_text(
                                "visual/measure/capture.json",
                                capture.to_string(),
                            ),
                            TaskArtifact::from_text(
                                "visual/measure/settle.json",
                                settle.to_string(),
                            ),
                        ],
                        ..Default::default()
                    },
                )
                .unwrap();
        }
    }
    let readout = runtime.experiment_readout(&experiment.id, None).unwrap();
    let metric = readout
        .metrics
        .iter()
        .find(|metric| metric.metric == "visual_landed_mismatches")
        .unwrap();
    for arm in &metric.arms {
        assert_eq!(
            arm.estimate.n, 1,
            "unavailable comparison is not a zero result: {}",
            arm.arm
        );
        assert_eq!(arm.unmeasured, 3);
        assert_eq!(arm.estimate.mean, Some(0.0));
    }
}

#[test]
fn missing_visual_reviewer_result_is_unavailable() {
    use orbit_engine::RuntimeHost;
    if !crate::dispatch_admission::isolated(
        "experiment_readout::visual::missing_visual_reviewer_result_is_unavailable",
    ) {
        return;
    }
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("global");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let output = runtime.run_deterministic("visual_verify_prepare", &json!({}), &json!({
        "workspace_path":workspace.parent().unwrap(),
        "prepared":{"records":[{"task_id":"TST-1","outcome":"ready","budget":{"max_findings":1}}]},
        "review_results":[]
    }), orbit_tools::ToolContext::default()).unwrap();
    assert_eq!(
        output["records"][0]["outcome"], "not_established",
        "an absent reviewer result cannot become a clean reviewed sample"
    );
}
