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

#[test]
fn visual_verifier_receives_rule_findings_and_excerpts_in_both_rounds() {
    use orbit_engine::RuntimeHost;
    if !crate::dispatch_admission::isolated(
        "experiment_readout::visual::visual_verifier_receives_rule_findings_and_excerpts_in_both_rounds",
    ) {
        return;
    }
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("global");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let rules = json!([{"path":"theme.md","content":"# Labels\nKeep labels readable."}]);
    let output = runtime.run_deterministic("visual_verify_prepare", &json!({}), &json!({
        "workspace_path":workspace.parent().unwrap(),
        "prepared":{"records":[{"task_id":"TST-1","outcome":"ready", "verifier_crew":"v", "escalation_crew":"e", "catalog":{"visual/capture.png":[100,100]}}],
            "reviews":[{"task_id":"TST-1","design_rules":rules}]},
        "review_results":[{"task_id":"TST-1","findings":[{"question":9,"mock":"mocks/view.png","title":"Label", "detail":"design_rules: Labels", "severity":"medium", "evidence":[{"image":"visual/capture.png","region":[0,0,10,10]}]}]}]
    }), orbit_tools::ToolContext::default()).unwrap();
    assert_eq!(output["partitions"][0]["findings"][0]["question"], "9");
    assert_eq!(output["partitions"][0]["design_rules"], rules);
    let escalation = runtime.run_deterministic("visual_verify_prepare", &json!({}), &json!({
        "workspace_path":workspace.parent().unwrap(), "round":"escalation", "prepared":output,
        "verify_results":[{"task_id":"TST-1","verdicts":[{"finding_id":"f1","verdict":"CONFIRMED","confidence":"low","severity":"medium","reasoning":"uncertain", "evidence":[{"location":"visual/capture.png#0,0,10,10","note":"label"}]}]}]
    }), orbit_tools::ToolContext::default()).unwrap();
    assert_eq!(escalation["partitions"][0]["design_rules"], rules);
}

#[test]
fn visual_review_prepare_caps_design_rule_excerpts_at_total_budget() {
    if !crate::dispatch_admission::isolated(
        "experiment_readout::visual::visual_review_prepare_caps_design_rule_excerpts_at_total_budget",
    ) {
        return;
    }
    use orbit_core::application::task::TaskAddParams;
    use orbit_core::{TaskComplexity, TaskStatus};
    use orbit_engine::RuntimeHost;
    use orbit_store::contracts::TaskArtifactUpdateParams;
    use orbit_store::maintenance::task_registry::{TaskRegistryStore, task_registry_path};
    use orbit_types::task::{ArtifactWriter, TaskArtifact};

    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global = root.path().join("global");
    let repo = root.path().join("repo");
    let workspace = repo.join(".orbit");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(
        workspace.join("config.toml"),
        "[visual_review]\nmode = \"shadow\"\nmanifests = [\"views.toml\"]\n\n\
         [workflow]\ndefault_crew = \"reviewer\"\n\n\
         [workflow.purpose_crews]\nvisual_review = [\"reviewer\"]\n\n\
         [crews.reviewer]\nprovider = \"claude\"\nmodel = \"fixture-reviewer\"\n",
    )
    .unwrap();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .current_dir(&repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    };
    git(&["init", "--quiet", "-b", "main"]);
    git(&["config", "user.name", "Orbit Test"]);
    git(&["config", "user.email", "orbit-test@example.com"]);
    git(&["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.join(".gitignore"), ".orbit/\n").unwrap();

    let paths = ["rules/short.md", "rules/large-a.md", "rules/large-b.md"];
    let short = "a".repeat(1_024);
    let large_a = "b".repeat(8_192);
    // The combined limit leaves 7,168 bytes. Its last byte would split é;
    // truncation must stop before that character to keep valid UTF-8.
    let large_b = format!("{}é{}", "c".repeat(7_167), "d".repeat(1_023));
    assert_eq!(large_b.len(), 8_192);
    std::fs::create_dir_all(repo.join("rules")).unwrap();
    for (path, content) in paths.iter().zip([&short, &large_a, &large_b]) {
        std::fs::write(repo.join(path), content).unwrap();
    }
    let manifest = format!(
        "schema_version = 1\n\n[app.web]\nthemes = [\"light\"]\ndesign_rules = {:?}\n\n\
         [[view]]\nid = \"checkout\"\napp = \"web\"\nstates = [\"default\"]\n",
        paths
    );
    std::fs::write(repo.join("views.toml"), manifest).unwrap();
    git(&["add", "-A"]);
    git(&["commit", "--quiet", "-m", "seed visual review manifest"]);
    let candidate_sha = git(&["rev-parse", "HEAD"]);

    // The contract pins to the commit above, even when the live file changes.
    std::fs::write(repo.join(paths[0]), "working tree version").unwrap();
    let runtime = OrbitRuntime::from_roots(&global, &workspace).unwrap();
    let task = runtime
        .add_task(TaskAddParams {
            title: "Review checkout visuals".into(),
            tags: vec!["verify-mock-ui".into()],
            status: Some(TaskStatus::Proposed),
            complexity: TaskComplexity::Low,
            ..TaskAddParams::default()
        })
        .unwrap();
    let task_id = task.id.to_string();
    let registry = TaskRegistryStore::open(&task_registry_path(&runtime.global_root())).unwrap();
    let stores = orbit_store::compose::workspace_coordinated_backends(
        registry,
        runtime.workspace_id().unwrap(),
        runtime.sqlite_store().unwrap(),
    )
    .unwrap()
    .task;
    let png = || {
        let mut bytes = vec![0; 24];
        bytes[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        bytes[19] = 1;
        bytes[23] = 1;
        bytes
    };
    let mock = "mocks/checkout.png";
    let capture = "visual/checkout/capture.png";
    let side_by_side = "visual/checkout/side-by-side.png";
    let capture_record = json!({
        "schema_version": 1,
        "outcome": "captured",
        "mode": "shadow",
        "task_id": task_id.clone(),
        "host": "fixture",
        "candidate_sha": candidate_sha.clone(),
        "manifests": [],
        "mocks": [{
            "artifact": mock,
            "sha256": "0".repeat(64),
            "view": "checkout",
            "state": "default",
            "theme": "light",
            "width": 1,
            "capture": {
                "path": capture,
                "side_by_side": side_by_side,
                "side_by_side_sha256": "0".repeat(64),
                "commit": candidate_sha,
                "viewport": [1, 1],
                "device_scale_factor": 1,
                "fixture": "default",
                "fixture_sha256": "0".repeat(64),
                "captured_at": "2026-11-01T00:00:00Z",
                "sha256": "0".repeat(64),
                "stability": "stable",
                "encoding": "png",
                "lossy": false
            }
        }],
        "over_budget": [],
        "not_established": []
    });
    let binary_artifact = |path: &str| TaskArtifact {
        path: path.into(),
        content: png(),
        media_type: "image/png".into(),
        created_by: None,
    };
    stores
        .artifact
        .upsert_task_artifacts(
            &task_id,
            TaskArtifactUpdateParams {
                actor: "system:fixture".into(),
                writer: Some(ArtifactWriter::System),
                upsert_artifacts: vec![
                    binary_artifact(mock),
                    binary_artifact(capture),
                    binary_artifact(side_by_side),
                    TaskArtifact::from_text("visual/capture.json", capture_record.to_string()),
                ],
                ..Default::default()
            },
        )
        .unwrap();
    runtime.approve_task(&task_id, None, None).unwrap();

    let prepared = runtime
        .run_deterministic(
            "visual_review_prepare",
            &json!({}),
            &json!({
                "workspace_path": repo,
                "capture": {"captures": [{"task_id": task_id}]}
            }),
            orbit_tools::ToolContext::default(),
        )
        .unwrap();
    let excerpts = prepared["reviews"][0]["design_rules"].as_array().unwrap();
    assert_eq!(
        excerpts.len(),
        paths.len(),
        "the public review dispatch includes each configured excerpt"
    );
    assert_eq!(
        excerpts
            .iter()
            .map(|excerpt| excerpt["path"].as_str().unwrap())
            .collect::<Vec<_>>(),
        paths,
        "review excerpts follow manifest order"
    );
    let contents = excerpts
        .iter()
        .map(|excerpt| excerpt["content"].as_str().unwrap())
        .collect::<Vec<_>>();
    let total_bytes = contents.iter().map(|content| content.len()).sum::<usize>();
    assert!(
        total_bytes <= 16_384,
        "combined design-rule excerpts must stay within 16 KiB: {total_bytes} bytes"
    );
    assert_eq!(
        contents[0], short,
        "read the short file at the pinned revision"
    );
    assert_eq!(contents[1], large_a);
    assert_eq!(contents[2], "c".repeat(7_167));
    assert!(contents.iter().all(|content| content.len() <= 8_192));
}
