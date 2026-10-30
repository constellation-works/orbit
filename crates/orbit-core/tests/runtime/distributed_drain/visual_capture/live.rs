use super::*;

/// A committed follow-up whose artifact write was interrupted is completed
/// through the public deterministic action, without another task or verdict.
fn visual_followup_retry_completes_existing_task_evidence() {
    use orbit_store::contracts::{TaskArtifactUpdateParams, TaskCreateParams};
    use orbit_store::maintenance::task_registry::{TaskRegistryStore, task_registry_path};
    use orbit_types::task::{
        ArtifactWriter, TaskComplexity, TaskPriority, TaskRelation, TaskRelationType, TaskStatus,
        TaskType,
    };
    let (pair, _, _) = hand_off(Returned::Matching, true);
    let runtime = &pair.wire.owner;
    let task = &pair.tasks[0];
    let registry = TaskRegistryStore::open(&task_registry_path(&runtime.global_root())).unwrap();
    let stores = orbit_store::compose::workspace_coordinated_backends(
        registry,
        runtime.workspace_id().unwrap(),
        runtime.sqlite_store().unwrap(),
    )
    .unwrap()
    .task;
    let capture = runtime
        .get_task_artifact(task, "visual/capture.json")
        .unwrap()
        .unwrap();
    let capture: Value = serde_json::from_slice(&capture.content).unwrap();
    let head = capture["candidate_sha"].as_str().unwrap();
    let title = "Visual review: missing action (mocks/checkout.png)";
    let key = format!("visual-follow-up:{task}:{head}:f1");
    let params = TaskCreateParams {
        actor: "system:fixture".into(),
        parent_id: None,
        title: title.into(),
        description: format!(
            "The visual review of {task} confirmed this low finding on mock `mocks/checkout.png` (question 1) at candidate {head}, and the delivery landed without fixing it. It was an optional fix the code reviewer did not make.\n\nabsent\n\nThe mock, the capture and the side-by-side are attached."
        ),
        acceptance_criteria: vec![
            "The view `mocks/checkout.png` renders matches its mock: missing action".into(),
        ],
        dependencies: vec![],
        relations: vec![TaskRelation {
            relation_type: TaskRelationType::SpawnedFrom,
            target: task.clone(),
            regression: None,
        }],
        tags: vec!["visual-review".into()],
        required_tools: vec![],
        granted_tools: vec![],
        requires_labels: vec![],
        not_before: None,
        plan: String::new(),
        execution_summary: String::new(),
        context_files: vec![],
        repo_root: None,
        created_by: Some("system:fixture".into()),
        created_by_model: None,
        planned_by: None,
        implemented_by: None,
        status: TaskStatus::Proposed,
        priority: TaskPriority::Low,
        complexity: Some(TaskComplexity::Low),
        task_type: TaskType::Bug,
        external_refs: vec![],
        source_task_id: None,
        crew: None,
        crew_source: None,
        orchestrator: None,
        orchestrator_model: None,
        orchestrator_model_source: None,
        origin: None,
        rule_fingerprint: None,
        comments: vec![],
        context_creation: vec![],
        creation_decision: None,
    };
    let (existing, replayed) = stores.task.create_task_idempotent(params, &key).unwrap();
    assert!(!replayed);
    let review = json!({"schema_version":1,"purpose":"visual_review","mode":"live",
        "task_id":task,"candidate_sha":head,"outcome":"reviewed","recorded_only":false,
        "budget":{"max_mocks":1,"max_images":2,"max_image_tokens":1000,"max_findings":1,
            "images_shown":2,"estimated_image_tokens":1000},
        "invalid_evidence_dropped":0,"over_max_findings_dropped":0,
        "findings":[{"finding_id":"f1","source":"comparison","question":"1",
            "mock":"mocks/checkout.png","title":"missing action","detail":"absent",
            "severity":"low","verdict":{"verdict":"confirmed","reasoning":"verified"},
            "disposition":"optional","resolution":"unfixed"}]});
    stores
        .artifact
        .upsert_task_artifacts(
            task,
            TaskArtifactUpdateParams {
                actor: "system:fixture".into(),
                writer: Some(ArtifactWriter::System),
                upsert_artifacts: vec![TaskArtifact::from_text(
                    "visual/review.json",
                    review.to_string(),
                )],
                ..Default::default()
            },
        )
        .unwrap();
    for status in [TaskStatus::Review, TaskStatus::Done] {
        runtime
            .apply_task_automation_update(
                task,
                orbit_engine::TaskAutomationUpdate {
                    status: Some(status),
                    ..Default::default()
                },
            )
            .unwrap();
    }
    let before = runtime.list_task_metadata().unwrap().len();
    for _ in 0..2 {
        runtime
            .run_deterministic(
                "visual_landed",
                &json!({}),
                &json!({"completed_task_ids":[task],"base":"main"}),
                ToolContext::default(),
            )
            .unwrap();
        for path in [
            "mocks/checkout.png",
            "visual/checkout/capture.png",
            "visual/checkout/side-by-side.png",
        ] {
            assert!(
                runtime
                    .get_task_artifact(&existing.id, path)
                    .unwrap()
                    .is_some(),
                "retry must complete {path}"
            );
        }
        assert_eq!(
            runtime
                .get_task_history(&existing.id)
                .unwrap()
                .iter()
                .filter(|entry| entry.event == "finding_verified")
                .count(),
            1
        );
        assert_eq!(runtime.list_task_metadata().unwrap().len(), before);
    }
    // An interrupted retry must preserve an operator's changed brief rather
    // than attach old evidence as a verification of the new requested work.
    runtime
        .update_task_as_human(
            &existing.id,
            orbit_core::application::task::TaskUpdateParams {
                description: Some("Operator changed the requested repair".into()),
                ..Default::default()
            },
            "human:fixture".into(),
        )
        .unwrap();
    stores
        .artifact
        .upsert_task_artifacts(
            task,
            TaskArtifactUpdateParams {
                actor: "system:fixture".into(),
                writer: Some(ArtifactWriter::System),
                upsert_artifacts: vec![TaskArtifact::from_text(
                    "visual/review.json",
                    review.to_string(),
                )],
                ..Default::default()
            },
        )
        .unwrap();
    let result = runtime.run_deterministic(
        "visual_landed",
        &json!({}),
        &json!({"completed_task_ids":[task],"base":"main"}),
        ToolContext::default(),
    );
    assert!(
        result.is_err(),
        "edited keyed material must hold rather than be reverified"
    );
    assert_eq!(
        runtime.get_task(&existing.id).unwrap().description,
        "Operator changed the requested repair"
    );
    assert_eq!(runtime.list_task_metadata().unwrap().len(), before);
}

/// Captures alone do not prove an enforcing final-candidate resolution.
fn live_claim_without_owner_visual_resolution_proof_is_held() {
    let config = CONFIG.replace("shadow", "live");
    let (pair, _kit) = prepared_mock_pair(&config);
    let drain = pair.run_drain();
    let pass = pair.pass_with(&drain, 1);
    assert_eq!(
        pass["admitted"], 0,
        "unsupported live proof must not mint a leaf: {pass}"
    );
    assert_eq!(pair.owner_status(&pair.tasks[0]), "backlog");
    let admissions = pair.follower_jobs.local_pull_admissions().unwrap();
    assert!(
        admissions
            .iter()
            .all(
                |admission| admission.receipt.as_ref().is_some_and(|receipt| receipt
                    .claim
                    .is_none()
                    && receipt
                        .deferred_conflicts
                        .iter()
                        .any(|hold| hold.task_id == pair.tasks[0]
                            && hold.reason.starts_with("visual_live_claim_unsupported:")))
                    && admission.leaf_run_id.is_none()
                    && admission.phase == LocalPullPhase::Idle
            ),
        "live admission must be idle: {admissions:?}"
    );
}

fn system_records(runtime: &OrbitRuntime, task: &str, artifacts: Vec<TaskArtifact>) {
    use orbit_store::contracts::TaskArtifactUpdateParams;
    use orbit_store::maintenance::task_registry::{TaskRegistryStore, task_registry_path};
    let registry = TaskRegistryStore::open(&task_registry_path(&runtime.global_root())).unwrap();
    let stores = orbit_store::compose::workspace_coordinated_backends(
        registry,
        runtime.workspace_id().unwrap(),
        runtime.sqlite_store().unwrap(),
    )
    .unwrap()
    .task;
    stores
        .artifact
        .upsert_task_artifacts(
            task,
            TaskArtifactUpdateParams {
                actor: "system:fixture".into(),
                writer: Some(orbit_types::task::ArtifactWriter::System),
                upsert_artifacts: artifacts,
                ..Default::default()
            },
        )
        .unwrap();
}

/// The experiment's control disables comparison review without disabling
/// final capture enforcement; treatment still holds an unavailable reviewer.
fn experiment_reviewer_outcome_controls_live_recapture() {
    use orbit_core::application::experiment::{
        ExperimentActor, ExperimentChange, ExperimentDefinitionInput,
    };
    for (arm, expected_outcome) in [
        ("control", "reviewer_disabled"),
        ("treatment", "not_established"),
    ] {
        let config = format!(
            "{}\n[experiments]\nenabled = true\nconfig_keys = [\"visual_review.reviewer\"]\n",
            CONFIG.replace("shadow", "live")
        );
        let (pair, _kit) = prepared_mock_pair(&config);
        let runtime = &pair.wire.owner;
        let task = &pair.tasks[0];
        let actor = ExperimentActor {
            actor: "visual-fixture".into(),
            reason: None,
        };
        let experiment = runtime.create_experiment("visual-fixture", ExperimentDefinitionInput {
            arms: Some(json!([
                {"name":"control", "control":true, "overlay":{"config":{"values":{"visual_review.reviewer":false}}}},
                {"name":"treatment", "overlay":{"config":{"values":{"visual_review.reviewer":true}}}}
            ])),
            primary_metric: Some("visual_landed_mismatches".into()),
            mode: Some("live".into()),
            ..Default::default()
        }, actor.clone()).unwrap();
        runtime
            .change_experiment(&experiment.id, ExperimentChange::Start, actor)
            .unwrap();
        runtime
            .force_experiment_assignment(&experiment.id, task, arm)
            .unwrap();
        let head = git2::Repository::open(&pair.owner_repo)
            .unwrap()
            .head()
            .unwrap()
            .target()
            .unwrap()
            .to_string();
        let capture = json!({"schema_version":1,"outcome":"captured","mode":"live","task_id":task,
            "host":"fixture","candidate_sha":head,"manifests":[],"mocks":[{
                "artifact":"mocks/checkout.png","sha256":mock_sha256_hex(MOCK),"view":"checkout",
                "state":"default","theme":"light","width":1440,
                "capture":{"path":"visual/checkout/capture.png","side_by_side":"visual/checkout/side-by-side.png",
                    "side_by_side_sha256":mock_sha256_hex(SIDE_BY_SIDE),"commit":head,"viewport":[1440,900],
                    "device_scale_factor":1,"fixture":"default","fixture_sha256":"0".repeat(64),
                    "captured_at":"2026-10-29T00:00:00Z","sha256":mock_sha256_hex(CAPTURE),
                    "stability":"stable","encoding":"png","lossy":false}}]});
        system_records(
            runtime,
            task,
            vec![TaskArtifact::from_text(
                "visual/capture.json",
                capture.to_string(),
            )],
        );
        let captures = json!({"captures":[{"task_id":task}]});
        let prepared = runtime
            .run_deterministic(
                "visual_review_prepare",
                &json!({}),
                &json!({"workspace_path":pair.owner_repo,"capture":captures}),
                ToolContext::default(),
            )
            .unwrap();
        assert_eq!(prepared["records"][0]["outcome"], expected_outcome);
        if arm == "treatment" {
            assert_eq!(
                prepared["records"][0]["not_established"][0]["reason"],
                "no_vision_crew"
            );
        }
        runtime
            .run_deterministic(
                "visual_settle",
                &json!({}),
                &json!({"workspace_path":pair.owner_repo,"capture":captures,"verified":prepared}),
                ToolContext::default(),
            )
            .unwrap();
        let result = runtime.run_deterministic(
            "visual_recapture",
            &json!({}),
            &json!({"workspace_path":pair.owner_repo,"completed_task_ids":[task]}),
            ToolContext::default(),
        );
        if arm == "control" {
            assert!(
                result.is_ok(),
                "disabled reviewer with complete capture must pass: {result:?}"
            );
        } else {
            let error = result
                .expect_err("unavailable reviewer must hold")
                .to_string();
            assert!(
                error.contains("the comparison review is not established"),
                "{error}"
            );
        }
    }
}

crate::scenario_table!(
    experiment_visual_recapture_regression,
    experiment_reviewer_outcome_controls_live_recapture
);

fn changed_live_candidate_without_final_evidence_is_held() {
    let (pair, _kit) = prepared_mock_pair(&CONFIG.replace("shadow", "live"));
    let runtime = &pair.wire.owner;
    let task = &pair.tasks[0];
    let original = git2::Repository::open(&pair.owner_repo)
        .unwrap()
        .head()
        .unwrap()
        .target()
        .unwrap()
        .to_string();
    std::fs::write(pair.owner_repo.join("change.txt"), "changed candidate").unwrap();
    git(&pair.owner_repo, &["add", "change.txt"]);
    git(&pair.owner_repo, &["commit", "-q", "-m", "changed"]);
    let mut escaped = Vec::new();
    for missing in [true, false] {
        if !missing {
            let review = json!({"schema_version":1,"purpose":"visual_review","mode":"live",
                "task_id":task,"candidate_sha":original,"outcome":"reviewed","recorded_only":false,
                "budget":{"max_mocks":1,"max_images":2,"max_image_tokens":1000,"max_findings":1,
                    "images_shown":2,"estimated_image_tokens":1000},
                "invalid_evidence_dropped":0,"over_max_findings_dropped":0,"findings":[]});
            system_records(
                runtime,
                task,
                vec![TaskArtifact::from_text(
                    "visual/review.json",
                    review.to_string(),
                )],
            );
        }
        let result = runtime.run_deterministic(
            "visual_recapture",
            &json!({}),
            &json!({"workspace_path":pair.owner_repo,"completed_task_ids":[task]}),
            ToolContext::default(),
        );
        if result.is_ok() {
            escaped.push(missing);
        }
    }
    assert!(
        escaped.is_empty(),
        "missing/stale live evidence must hold, missing review: {escaped:?}"
    );
}

fn measurement_capture_uses_the_durable_landed_revision() {
    let (pair, _kit) = prepared_mock_pair(CONFIG);
    let runtime = &pair.wire.owner;
    let task = &pair.tasks[0];
    let landed = git2::Repository::open(&pair.owner_repo)
        .unwrap()
        .head()
        .unwrap()
        .target()
        .unwrap()
        .to_string();
    std::fs::write(pair.owner_repo.join("later.txt"), "later checkout").unwrap();
    git(&pair.owner_repo, &["add", "later.txt"]);
    git(&pair.owner_repo, &["commit", "-q", "-m", "later"]);
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
    let mut state = PipelineState::new(run.run_id.clone(), run.job_id, json!({"task_ids":[task]}));
    let output = json!({"phase":"complete","merge":{"merged":true,"landed_commit":landed},"completed_task_ids":[task]});
    state.record_step(
        0,
        JobRunState::Success,
        Some(output.clone()),
        Some(json!({"complete_pr":output})),
    );
    runtime.write_run_state(&run.run_id, &state).unwrap();
    let output = runtime.run_deterministic("visual_capture", &json!({}),
        &json!({"workspace_path":pair.owner_repo,"completed_task_ids":[task],"job_run_id":run.run_id,"pass":"measure"}), ToolContext::default()).unwrap();
    assert_eq!(
        output["captures"][0]["candidate_sha"], landed,
        "measurement must not use a later checkout HEAD"
    );
    runtime
        .run_deterministic(
            "visual_settle",
            &json!({}),
            &json!({"workspace_path":pair.owner_repo,"capture":output,"pass":"measure"}),
            ToolContext::default(),
        )
        .unwrap();
    let artifact = runtime
        .get_task_artifact(task, "visual/measure/settle.json")
        .unwrap()
        .unwrap();
    let settled: VisualSettleRecord = serde_json::from_slice(&artifact.content).unwrap();
    assert_eq!(
        settled.candidate_sha, landed,
        "measurement settlement must retain the landed identity"
    );
    assert!(
        settled
            .refusals
            .iter()
            .all(|refusal| refusal.code != "stale_capture"),
        "a later checkout must not invalidate the landed measurement"
    );
}

crate::scenario_table!(
    live_visual_delivery_regressions,
    live_claim_without_owner_visual_resolution_proof_is_held,
    visual_followup_retry_completes_existing_task_evidence
);

crate::scenario_table!(
    final_visual_evidence_regression,
    changed_live_candidate_without_final_evidence_is_held
);
crate::scenario_table!(
    landed_measurement_regression,
    measurement_capture_uses_the_durable_landed_revision
);

/// A verified browser kit under `home`: its pixel helper at the pinned
/// version, recorded in the kit's verification.
#[cfg(unix)]
fn install_browser_kit(home: &Path) {
    crate::host_ui_routing::write_kit(home, true);
    let layout = orbit_core::bootstrap::browser_kit::KitLayout::for_home(home);
    let helper = layout
        .npm_project()
        .join("node_modules")
        .join(orbit_exec::browser_kit::PIXEL_HELPER_PACKAGE);
    std::fs::create_dir_all(&helper).unwrap();
    std::fs::write(
        helper.join("package.json"),
        json!({"version":orbit_exec::browser_kit::PIXEL_HELPER_VERSION}).to_string(),
    )
    .unwrap();
    let mut verification: Value =
        serde_json::from_slice(&std::fs::read(layout.verification_file()).unwrap()).unwrap();
    verification["pixel_helper_version"] = json!(orbit_exec::browser_kit::PIXEL_HELPER_VERSION);
    std::fs::write(layout.verification_file(), verification.to_string()).unwrap();
}

/// The external browser process: a `node` on the scenario PATH that prints
/// `driver` as the capture driver's report. `driver` must hold no shell quotes.
#[cfg(unix)]
fn write_fake_driver(home: &Path, driver: &Value) {
    use std::os::unix::fs::PermissionsExt;
    let bin = home.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let node = bin.join("node");
    std::fs::write(&node, format!("#!/bin/sh\nprintf '%s\\n' '{}'\n", driver)).unwrap();
    std::fs::set_permissions(&node, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// The real pre-landing action must hold unavailable and partial recaptures,
/// then accept a complete capture of its final HEAD. The fixture substitutes
/// only the external browser process; Git, artifacts, checks and resolution
/// all run through their production boundaries.
#[cfg(unix)]
fn fixed_vision_finding_requires_complete_final_capture() {
    let (pair, _kit) = prepared_mock_pair(&CONFIG.replace("shadow", "live"));
    let runtime = &pair.wire.owner;
    let task = &pair.tasks[0];
    system_records(
        runtime,
        task,
        vec![TaskArtifact {
            path: "mocks/checkout.dark.png".into(),
            content: MOCK.to_vec(),
            media_type: "image/png".into(),
            created_by: None,
        }],
    );
    std::fs::write(
        pair.owner_repo.join("views.toml"),
        MANIFEST.replace("[app.web]", "[app.web]\nsetup = [\"true\"]"),
    )
    .unwrap();
    git(&pair.owner_repo, &["add", "views.toml"]);
    git(&pair.owner_repo, &["commit", "-q", "-m", "capture fixture"]);
    let repo = git2::Repository::open(&pair.owner_repo).unwrap();
    let original = repo.head().unwrap().target().unwrap().to_string();
    std::fs::write(pair.owner_repo.join("repair.txt"), "fixed action").unwrap();
    git(&pair.owner_repo, &["add", "repair.txt"]);
    git(&pair.owner_repo, &["commit", "-q", "-m", "repair"]);
    let head = repo.head().unwrap().target().unwrap().to_string();
    let finding = json!({"finding_id":"f1","source":"comparison","question":"1","mock":"mocks/checkout.png",
        "title":"missing action","detail":"absent","severity":"high",
        "verdict":{"verdict":"confirmed","reasoning":"verified"},"disposition":"must_address"});
    let review = json!({"schema_version":1,"purpose":"visual_review","mode":"live","task_id":task,
        "candidate_sha":original,"outcome":"reviewed","recorded_only":false,
        "budget":{"max_mocks":2,"max_images":4,"max_image_tokens":1000,"max_findings":1,
            "images_shown":4,"estimated_image_tokens":1000},
        "invalid_evidence_dropped":0,"over_max_findings_dropped":0,"findings":[finding]});
    let manifest = json!({"schema_version":1,"attempt_id":"rvw-fixture","lineage_key":"fixture","task_ids":[task],
        "task_digests":{},"task_meaning_digest":"fixture","repository":"fixture",
        "base":{"commit":original,"tree":"base"},"candidate":{"commit":original,"tree":"candidate"},
        "implementation_commits":[],"implementer_summaries":{},"reviewer_crew":"fixture",
        "contract_version":1,"policy_version":1,"budget":{"minutes":1},"remaining":{"seconds":60},
        "issued_at":Utc::now(),"visual_findings":[{"finding_id":"f1","mock":"mocks/checkout.png","question":"1",
            "title":"missing action","detail":"absent","severity":"high","must_address":true,"images":[]}]});
    let mut report = json!({"schema_version":1,"attempt_id":"rvw-fixture","verdict":"accept","summary":"fixed",
        "visual_resolutions":[{"finding_id":"f1","resolution":"fixed"}]});
    system_records(
        runtime,
        task,
        vec![
            TaskArtifact::from_text("visual/review.json", review.to_string()),
            TaskArtifact::from_text(
                orbit_types::workflow::REVIEW_MANIFEST_ARTIFACT,
                manifest.to_string(),
            ),
            TaskArtifact::from_text(
                orbit_types::workflow::REVIEW_REPORT_ARTIFACT,
                report.to_string(),
            ),
        ],
    );
    let recapture = || {
        runtime.run_deterministic("visual_recapture", &json!({}),
        &json!({"workspace_path":pair.owner_repo,"completed_task_ids":[task],"job_run_id":"fixture"}), ToolContext::default())
    };
    assert!(
        recapture().is_err(),
        "missing kit must hold a fixed vision finding"
    );
    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
    install_browser_kit(&home);
    let out = pair
        .owner_repo
        .join(".orbit/tmp/visual-delivery/fixture")
        .join(task)
        .join("recapture/out");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("capture.png"), CAPTURE).unwrap();
    std::fs::write(out.join("side-by-side.png"), SIDE_BY_SIDE).unwrap();
    std::fs::write(out.join("rules.json"), r#"{"source":null,"results":[]}"#).unwrap();
    for partial in [true, false] {
        let mut entries = Vec::new();
        for (index, mock) in ["mocks/checkout.png", "mocks/checkout.dark.png"]
            .iter()
            .enumerate()
        {
            let mut entry = json!({"artifact":mock,"name":"checkout","view":"checkout","state":"default",
                "theme":if index == 0 {"light"} else {"dark"},"width":1440,"viewport":[1440,900],"fixture":"default",
                "capture":"capture.png","side_by_side":"side-by-side.png","stability":"stable"});
            if partial && index == 1 {
                entry["error"] = json!({"code":"setup_failed","message":"fixture failed"});
            }
            entries.push(entry);
        }
        let driver = json!({"outcome":"captured","kit":{},"frozen_instant":"2026-01-01T12:00:00.000Z",
            "captures_per_mock":2,"mocks":entries,"rules":"rules.json","rules_summary":{}});
        write_fake_driver(&home, &driver);
        let result = recapture();
        if partial {
            assert!(
                result.is_err(),
                "one rendered mock cannot establish a partial recapture"
            );
        } else {
            assert!(
                result.is_ok(),
                "complete final-candidate recapture: {result:?}; capture={:?}; checks={:?}",
                runtime
                    .get_task_artifact(task, "visual/recapture/capture.json")
                    .unwrap()
                    .map(|artifact| String::from_utf8_lossy(&artifact.content).into_owned()),
                runtime
                    .get_task_artifact(task, "visual/recapture/checks.json")
                    .unwrap()
                    .map(|artifact| String::from_utf8_lossy(&artifact.content).into_owned())
            );
        }
    }
    let stored = runtime
        .get_task_artifact(task, "visual/review.json")
        .unwrap()
        .unwrap();
    let resolved = orbit_types::task::VisualReviewRecord::parse(&stored.content).unwrap();
    assert!(resolved.unresolved_blocking().is_empty());
    assert_eq!(resolved.recapture.unwrap().candidate_sha, head);
    // A reviewer may dispute, but only a trusted operator accepts the dispute.
    report["visual_resolutions"][0]["resolution"] = json!("disputed");
    report["visual_resolutions"][0]["reason"] = json!("intentional action placement");
    system_records(
        runtime,
        task,
        vec![TaskArtifact::from_text(
            orbit_types::workflow::REVIEW_REPORT_ARTIFACT,
            report.to_string(),
        )],
    );
    assert!(recapture().is_err(), "an unaccepted dispute still holds");
    let input = json!({"id":task,"finding":"f1","reason":"operator accepts placement"});
    assert!(
        runtime
            .run_tool_with_context_and_role(
                "orbit.visual.accept_dispute",
                input.clone(),
                Role::Agent,
                ToolContext::default()
            )
            .is_err()
    );
    runtime
        .run_tool_with_context_and_role(
            "orbit.visual.accept_dispute",
            input,
            Role::Admin,
            ToolContext {
                session_context: ToolSessionContext {
                    effective_capabilities: BTreeSet::from([McpCapability::Operator]),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .expect("trusted operator accepts the dispute");
    recapture().expect("accepted operator dispute remains authoritative");
}

#[cfg(unix)]
crate::scenario_table!(
    fixed_visual_resolution_regression,
    fixed_vision_finding_requires_complete_final_capture
);

/// A live delivery with more frozen mocks than `visual_review.max_mocks` holds
/// only on evidence it lacks. The over-budget mock is listed over budget and
/// never captured, so it cannot hold a candidate whose budgeted mock rendered
/// at the final head; a hold for missing evidence names that cause.
#[cfg(unix)]
fn live_over_budget_mock_does_not_hold_the_budgeted_capture() {
    let config = format!("{}max_mocks = 1\n", CONFIG.replace("shadow", "live"));
    let (pair, _kit) = prepared_mock_pair(&config);
    let runtime = &pair.wire.owner;
    let task = &pair.tasks[0];
    // Sorted by artifact name, `mocks/checkout.dark.png` is the budgeted mock
    // and `mocks/checkout.png` is over budget.
    system_records(
        runtime,
        task,
        vec![TaskArtifact {
            path: "mocks/checkout.dark.png".into(),
            content: MOCK.to_vec(),
            media_type: "image/png".into(),
            created_by: None,
        }],
    );
    std::fs::write(
        pair.owner_repo.join("views.toml"),
        MANIFEST.replace("[app.web]", "[app.web]\nsetup = [\"true\"]"),
    )
    .unwrap();
    git(&pair.owner_repo, &["add", "views.toml"]);
    git(&pair.owner_repo, &["commit", "-q", "-m", "capture fixture"]);
    let head = git2::Repository::open(&pair.owner_repo)
        .unwrap()
        .head()
        .unwrap()
        .target()
        .unwrap()
        .to_string();
    let home = PathBuf::from(std::env::var_os("HOME").unwrap());
    install_browser_kit(&home);
    let out = pair
        .owner_repo
        .join(".orbit/tmp/visual-delivery/fixture")
        .join(task)
        .join("out");
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("capture.png"), CAPTURE).unwrap();
    std::fs::write(out.join("side-by-side.png"), SIDE_BY_SIDE).unwrap();
    let driver = |failed: bool| {
        let mut entry = json!({"artifact":"mocks/checkout.dark.png","name":"checkout","view":"checkout",
            "state":"default","theme":"dark","width":1440,"viewport":[1440,900],"fixture":"default",
            "capture":"capture.png","side_by_side":"side-by-side.png","stability":"stable"});
        if failed {
            entry["error"] = json!({"code":"render_timeout","message":"render timed out"});
        }
        json!({"outcome":"captured","kit":{},"frozen_instant":"2026-01-01T12:00:00.000Z",
            "captures_per_mock":2,"mocks":[entry],"rules":"rules.json","rules_summary":{}})
    };
    let capture = || {
        runtime.run_deterministic(
            "visual_capture",
            &json!({}),
            &json!({"workspace_path":pair.owner_repo,"completed_task_ids":[task],"job_run_id":"fixture"}),
            ToolContext::default(),
        )
    };
    let recapture = || {
        runtime.run_deterministic(
            "visual_recapture",
            &json!({}),
            &json!({"workspace_path":pair.owner_repo,"completed_task_ids":[task]}),
            ToolContext::default(),
        )
    };
    let review = json!({"schema_version":1,"purpose":"visual_review","mode":"live","task_id":task,
        "candidate_sha":head,"outcome":"reviewed","recorded_only":false,
        "budget":{"max_mocks":1,"max_images":2,"max_image_tokens":1000,"max_findings":1,
            "images_shown":2,"estimated_image_tokens":1000},
        "invalid_evidence_dropped":0,"over_max_findings_dropped":0,"findings":[]});
    system_records(
        runtime,
        task,
        vec![TaskArtifact::from_text(
            "visual/review.json",
            review.to_string(),
        )],
    );
    write_fake_driver(&home, &driver(false));
    capture().expect("capture the budgeted mock");
    let stored = runtime
        .get_task_artifact(task, "visual/capture.json")
        .unwrap()
        .unwrap();
    let stored: Value = serde_json::from_slice(&stored.content).unwrap();
    assert_eq!(
        stored["outcome"], "captured",
        "the budgeted mock must render: {stored}"
    );
    assert_eq!(
        stored["over_budget"],
        json!(["mocks/checkout.png"]),
        "the over-budget mock is recorded, not captured: {stored}"
    );
    recapture().expect("a budgeted mock captured at the final head must not hold the delivery");
    write_fake_driver(&home, &driver(true));
    capture().expect("capture the failed render");
    let error = recapture()
        .expect_err("a budgeted mock without a capture holds the delivery")
        .to_string();
    assert!(error.contains("visual evidence"), "{error}");
    assert!(
        !error.contains("confirmed visual findings"),
        "an evidence hold must name its cause, not unresolved findings: {error}"
    );
}

#[cfg(unix)]
crate::scenario_table!(
    live_over_budget_mock_regression,
    live_over_budget_mock_does_not_hold_the_budgeted_capture
);
