//! Public owner acceptance and landing boundaries for negotiated live claims.
use super::super::claimed_review::revision;
use super::*;
use orbit_engine::{HandoffLandingStep, HandoffLandingUpdate};
use orbit_types::task::{
    CLAIMED_VISUAL_RESOLUTION_SCHEMA, ClaimedVisualResolution, VISUAL_RESOLUTION_ARTIFACT,
};

struct LiveClaim {
    pair: Pair,
    _kit: TempDir,
    handoff: TaskHandoff,
    bundle: ClaimedVisualResolution,
    capture: Value,
    review: Value,
    sequence: std::cell::Cell<u64>,
}

impl LiveClaim {
    fn new() -> Self {
        Self::with_config("")
    }
    fn with_config(extra: &str) -> Self {
        let (pair, kit) =
            prepared_mock_pair(&format!("{}\n{extra}", CONFIG.replace("shadow", "live")));
        let drain = pair.run_drain();
        let leaf = pair.running_leaf(&drain, 1);
        let admission = pair.admission(&leaf);
        let contract = admission
            .receipt
            .as_ref()
            .unwrap()
            .visual_review
            .as_ref()
            .unwrap();
        assert_eq!(
            admission.request.visual_resolution,
            CLAIMED_VISUAL_RESOLUTION_SCHEMA
        );
        assert_eq!(
            contract.resolution_schema,
            Some(CLAIMED_VISUAL_RESOLUTION_SCHEMA)
        );
        let mut delivered = handoff(&admission);
        delivered.candidate.candidate = revision(&pair.owner_repo, "HEAD");
        let head = delivered.candidate.candidate.commit.clone();
        let capture = json!({"schema_version":1,"outcome":"captured","mode":"live","task_id":delivered.task_id,
            "host":FOLLOWER,"kit":{"playwright":PLAYWRIGHT_VERSION},"candidate_sha":head,
            "manifests":contract.manifests.iter().map(|manifest| &manifest.reference).collect::<Vec<_>>(),
            "mocks":[{"artifact":"mocks/checkout.png","sha256":mock_sha256_hex(MOCK),"view":"checkout",
                "state":"default","theme":"light","width":1440,
                "capture":{"path":"visual/checkout/capture.png","side_by_side":"visual/checkout/side-by-side.png",
                    "side_by_side_sha256":mock_sha256_hex(SIDE_BY_SIDE),"commit":head,"viewport":[1440,900],
                    "device_scale_factor":1,"fixture":"default","fixture_sha256":"0".repeat(64),
                    "captured_at":"2026-10-29T00:00:00Z","sha256":mock_sha256_hex(CAPTURE),
                    "stability":"stable","encoding":"png","lossy":false}}]});
        let review = json!({"schema_version":1,"purpose":"visual_review","mode":"live",
            "task_id":delivered.task_id,"candidate_sha":head,"outcome":"reviewed","recorded_only":false,
            "budget":{"max_mocks":12,"max_images":32,"max_image_tokens":48000,"max_findings":15,
                "images_shown":2,"estimated_image_tokens":1000},
            "invalid_evidence_dropped":0,"over_max_findings_dropped":0,"findings":[]});
        let checks = json!({"schema_version":1,"mode":"live","candidate_sha":head,"blocking":false,"findings":[]});
        let reference = |path: &str, value: &Value| HandoffArtifactRef {
            path: path.into(),
            sha256: mock_sha256_hex(&serde_json::to_vec(value).unwrap()),
        };
        let bundle = ClaimedVisualResolution {
            schema_version: 1,
            task_id: delivered.task_id.clone(),
            claim_id: delivered.claim_id.clone(),
            run_id: leaf,
            candidate_sha: head,
            contract_sha256: mock_sha256_hex(&serde_json::to_vec(contract).unwrap()),
            initial_capture: reference("visual/capture.json", &capture),
            final_capture: reference("visual/capture.json", &capture),
            final_checks: reference("visual/checks.json", &checks),
            review: reference("visual/review.json", &review),
        };
        let mut fixture = Self {
            pair,
            _kit: kit,
            handoff: delivered,
            bundle,
            capture,
            review,
            sequence: std::cell::Cell::new(0),
        };
        fixture.put(vec![
            TaskArtifact {
                path: "visual/checkout/capture.png".into(),
                content: CAPTURE.into(),
                media_type: "image/png".into(),
                created_by: None,
            },
            TaskArtifact {
                path: "visual/checkout/side-by-side.png".into(),
                content: SIDE_BY_SIDE.into(),
                media_type: "image/png".into(),
                created_by: None,
            },
            Self::artifact("visual/capture.json", &fixture.capture),
            Self::artifact("visual/checks.json", &checks),
            Self::artifact("visual/review.json", &fixture.review),
        ]);
        fixture.seal();
        fixture
    }
    fn worker_seals(&mut self) {
        let admission = self.pair.admission(&self.handoff.run_id);
        let claim = admission.receipt.unwrap().claim.unwrap();
        let bound = self
            .pair
            .follower
            .clone()
            .with_worker_invocation(
                orbit_types::tool::WorkerInvocation {
                    owner_machine_id: OWNER.into(),
                    owner_workspace_id: admission.destination.owner_workspace_id,
                    owner_destination: admission.destination.selector,
                    task_id: claim.task_id,
                    claim_id: claim.claim_id,
                    execution: claim.executed_on,
                    bound_run_id: self.handoff.run_id.clone(),
                },
                Arc::new(super::super::claimed_review::ToOwner(
                    self.pair.wire.owner.clone(),
                )),
            )
            .unwrap();
        let output = bound.run_deterministic("visual_recapture", &json!({}),
            &json!({"workspace_path":self.pair.owner_repo,"completed_task_ids":[self.handoff.task_id]}),ToolContext::default()).unwrap();
        assert_eq!(output["capture_artifact"], VISUAL_RESOLUTION_ARTIFACT);
        self.handoff.visual = Some(HandoffArtifactRef {
            path: output["capture_artifact"].as_str().unwrap().into(),
            sha256: output["capture_sha256"].as_str().unwrap().into(),
        });
        let stored = self
            .pair
            .wire
            .owner
            .get_task_artifact(&self.handoff.task_id, VISUAL_RESOLUTION_ARTIFACT)
            .unwrap()
            .unwrap();
        self.bundle = serde_json::from_slice(&stored.content).unwrap();
        self.review = serde_json::from_slice(
            &self
                .pair
                .wire
                .owner
                .get_task_artifact(&self.handoff.task_id, "visual/review.json")
                .unwrap()
                .unwrap()
                .content,
        )
        .unwrap();
        // Keep the mutable matrix's restoration bytes identical to its refs.
        let artifact = Self::artifact("visual/review.json", &self.review);
        self.bundle.review.sha256 = mock_sha256_hex(&artifact.content);
        self.put(vec![artifact]);
        self.seal();
    }
    fn artifact(path: &str, value: &impl serde::Serialize) -> TaskArtifact {
        TaskArtifact {
            path: path.into(),
            content: serde_json::to_vec(value).unwrap(),
            media_type: "application/json".into(),
            created_by: None,
        }
    }
    fn put(&self, artifacts: Vec<TaskArtifact>) {
        let sequence = self.sequence.get() + 1;
        self.sequence.set(sequence);
        let context = ClaimInvocation::trusted_worker(
            self.handoff.task_id.clone(),
            self.handoff.claim_id.clone(),
            self.handoff.machine_id.clone(),
            Some(ClaimRun {
                machine_id: self.handoff.machine_id.clone(),
                run_id: self.handoff.run_id.clone(),
            }),
        );
        self.pair
            .wire
            .owner
            .mutate_execution_claim(
                Some(&context),
                &format!("visual-evidence-{sequence}"),
                &ClaimMutation::Evidence(ClaimEvidence {
                    artifacts,
                    ..Default::default()
                }),
            )
            .unwrap();
    }
    fn seal(&mut self) {
        let artifact = Self::artifact(VISUAL_RESOLUTION_ARTIFACT, &self.bundle);
        self.handoff.visual = Some(HandoffArtifactRef {
            path: artifact.path.clone(),
            sha256: mock_sha256_hex(&artifact.content),
        });
        self.put(vec![artifact]);
    }
    fn accept(&self) -> Result<(), OrbitError> {
        self.pair.wire.accept(self.handoff.clone())
    }
    fn accepted(&self) -> orbit_types::workflow::handoff::AcceptedHandoff {
        self.pair
            .wire
            .owner
            .accepted_task_handoff(&self.handoff.claim_id)
            .unwrap()
    }
}

fn negotiated_live_claim_requires_complete_exact_candidate_resolution() {
    let mut live = LiveClaim::new();
    live.worker_seals();
    let original = live.bundle.clone();
    // Every variant reaches the public owner acceptance seam, with the
    // mutated bundle's digest recomputed so identity validation must decide.
    for case in [
        "missing",
        "schema",
        "claim",
        "run",
        "head",
        "contract",
        "review_digest",
        "partial",
        "wrong_image_commit",
        "disabled",
    ] {
        live.bundle = original.clone();
        match case {
            "missing" => live.handoff.visual = None,
            "schema" => live.bundle.schema_version = 99,
            "claim" => live.bundle.claim_id = "foreign".into(),
            "run" => live.bundle.run_id = "foreign".into(),
            "head" => live.bundle.candidate_sha = "f".repeat(40),
            "contract" => live.bundle.contract_sha256 = "0".repeat(64),
            "review_digest" => live.bundle.review.sha256 = "0".repeat(64),
            "partial" | "wrong_image_commit" => {
                let mut capture = live.capture.clone();
                if case == "partial" {
                    capture["mocks"][0]["capture"] = Value::Null;
                } else {
                    capture["mocks"][0]["capture"]["commit"] = json!("f".repeat(40));
                }
                let artifact = LiveClaim::artifact("visual/capture.json", &capture);
                live.bundle.initial_capture.sha256 = mock_sha256_hex(&artifact.content);
                live.bundle.final_capture = live.bundle.initial_capture.clone();
                live.put(vec![artifact]);
            }
            "disabled" => {
                let mut review = live.review.clone();
                review["outcome"] = json!("reviewer_disabled");
                let artifact = LiveClaim::artifact("visual/review.json", &review);
                live.bundle.review.sha256 = mock_sha256_hex(&artifact.content);
                live.put(vec![artifact]);
            }
            _ => unreachable!(),
        }
        if case != "missing" {
            live.seal();
        }
        let error = live.accept().expect_err(case).to_string();
        assert!(
            error.contains("delivery_visual_review_unresolved"),
            "{case}: {error}"
        );
        live.put(vec![
            LiveClaim::artifact("visual/capture.json", &live.capture),
            LiveClaim::artifact("visual/review.json", &live.review),
        ]);
    }
    live.bundle = original;
    live.seal();
    live.accept()
        .expect("complete negotiated evidence accepted");
    let accepted = live.accepted();
    RuntimeHost::handoff_landing_context(&live.pair.wire.owner, &accepted.handoff_id)
        .expect("verified live candidate can enter landing");
    let owner = &live.pair.wire.owner;
    let context = ClaimInvocation::trusted_operator(
        live.handoff.task_id.clone(),
        live.handoff.claim_id.clone(),
        "owner-landing".into(),
    )
    .with_handoff_observation(HandoffObservation {
        candidate: live.handoff.candidate.clone(),
        footprint_widening: vec![],
        required_commands: vec![],
        owner_completion_authority: None,
        review: None,
        known_red: vec![],
        light_delivery: vec![],
    });
    owner
        .mutate_execution_claim(
            Some(&context),
            "approve",
            &ClaimMutation::ApproveHandoff {
                handoff_id: accepted.handoff_id.clone(),
                candidate: live.handoff.candidate.clone(),
            },
        )
        .unwrap();
    owner
        .mutate_execution_claim(
            Some(&context),
            "dispatch",
            &ClaimMutation::DispatchLanding {
                handoff_id: accepted.handoff_id.clone(),
                job_run_id: None,
            },
        )
        .unwrap();
    let complete = HandoffLandingUpdate {
        handoff_id: accepted.handoff_id.clone(),
        step: HandoffLandingStep::Complete,
        observed: Some(live.handoff.candidate.clone()),
        evidence:
            json!({"delivery":{"merge_commit":"e".repeat(40),"pr_number":"42"},"pull_request":42})
                .to_string(),
    };
    // Alter stored evidence after acceptance: landing must independently
    // re-read it, rather than treating acceptance as a permanent pass.
    use orbit_store::maintenance::task_registry::{TaskRegistryStore, task_registry_path};
    let registry = TaskRegistryStore::open(&task_registry_path(&owner.global_root())).unwrap();
    let task_bundle = registry
        .canonical_task_bundle_path(&owner.workspace_id().unwrap(), &live.handoff.task_id)
        .unwrap();
    let file = owner
        .get_task_artifact_manifest(&live.handoff.task_id)
        .unwrap()
        .into_iter()
        .find(|entry| entry.path == "visual/review.json")
        .unwrap();
    let blob = task_bundle.join("artifacts").join(file.blob);
    let original_bytes = std::fs::read(&blob).unwrap();
    // Public writes are fenced after handoff. Simulate damaged storage,
    // then exercise public landing entry and completion against that damage.
    let manifest_path = task_bundle.join("artifacts/manifest.yaml");
    let original_manifest = std::fs::read(&manifest_path).unwrap();
    let mut manifest: orbit_types::task::ArtifactManifestV2 =
        serde_yaml::from_slice(&original_manifest).unwrap();
    let damaged = b"{\"stale\":true}";
    let entry = manifest
        .files
        .iter_mut()
        .find(|entry| entry.path == "visual/review.json")
        .unwrap();
    entry.sha256 = mock_sha256_hex(damaged);
    entry.size_bytes = damaged.len() as u64;
    std::fs::write(&blob, damaged).unwrap();
    std::fs::write(&manifest_path, serde_yaml::to_string(&manifest).unwrap()).unwrap();
    let error = RuntimeHost::handoff_landing_context(&live.pair.wire.owner, &accepted.handoff_id)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("delivery_visual_review_unresolved"),
        "{error}"
    );
    let error = RuntimeHost::record_handoff_landing(owner, &complete)
        .expect_err("completion rechecks proof")
        .to_string();
    assert!(
        error.contains("delivery_visual_review_unresolved"),
        "{error}"
    );
    std::fs::write(&blob, original_bytes).unwrap();
    std::fs::write(&manifest_path, original_manifest).unwrap();
    RuntimeHost::record_handoff_landing(owner, &complete)
        .expect("complete exact-candidate live delivery can land");
    assert_eq!(live.pair.owner_status(&live.handoff.task_id), "done");
}

fn worker_dispute_labels_cannot_replace_operator_or_review_authority() {
    let mut live = LiveClaim::with_config(
        "[review]\nbefore_landing = true\n[operation]\nreview_crew = \"sol\"\n",
    );
    live.review["findings"] = json!([{"finding_id":"f1","source":"comparison","question":"1","mock":"mocks/checkout.png",
        "title":"missing action","detail":"absent","severity":"high","verdict":{"verdict":"confirmed","reasoning":"verified"},
        "disposition":"must_address","resolution":"dispute_accepted"}]);
    let artifact = LiveClaim::artifact("visual/review.json", &live.review);
    live.bundle.review.sha256 = mock_sha256_hex(&artifact.content);
    live.put(vec![artifact]);
    live.seal();
    let error = live
        .accept()
        .expect_err("worker cannot accept its own dispute")
        .to_string();
    assert!(
        error.contains("delivery_visual_review_unresolved"),
        "{error}"
    );
    live.review["findings"][0]["resolution"] = json!("disputed");
    live.put(vec![LiveClaim::artifact(
        "visual/review.json",
        &live.review,
    )]);
    let owner = &live.pair.wire.owner;
    let input =
        json!({"id":live.handoff.task_id,"finding":"f1","reason":"operator accepts placement"});
    assert!(
        owner
            .run_tool_with_context_and_role(
                "orbit.visual.accept_dispute",
                input.clone(),
                Role::Agent,
                ToolContext::default()
            )
            .is_err()
    );
    // The active claim's write fence also applies to the operator. Its
    // ordinary release makes dispute acceptance available without granting
    // either the worker or the old claim any new landing authority.
    let worker = ClaimInvocation::trusted_worker(
        live.handoff.task_id.clone(),
        live.handoff.claim_id.clone(),
        live.handoff.machine_id.clone(),
        Some(ClaimRun {
            machine_id: live.handoff.machine_id.clone(),
            run_id: live.handoff.run_id.clone(),
        }),
    );
    owner
        .mutate_execution_claim(
            Some(&worker),
            "release-for-dispute",
            &ClaimMutation::Release(ClaimEvidence {
                summary: Some("held for operator dispute".into()),
                ..Default::default()
            }),
        )
        .unwrap();
    owner
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
        .unwrap();
    let stored = owner
        .get_task_artifact(&live.handoff.task_id, "visual/review.json")
        .unwrap()
        .unwrap();
    live.bundle.review.sha256 = mock_sha256_hex(&stored.content);
    let proof = LiveClaim::artifact(VISUAL_RESOLUTION_ARTIFACT, &live.bundle);
    live.handoff.visual.as_mut().unwrap().sha256 = mock_sha256_hex(&proof.content);
    super::live::system_records(owner, &live.handoff.task_id, vec![proof]);
    let error = live
        .accept()
        .expect_err(
            "operator's visual acceptance cannot revive a released claim or bypass code review",
        )
        .to_string();
    assert!(
        !error.contains("delivery_visual_review_unresolved"),
        "visual proof passes, ordinary claim/review gates still hold: {error}"
    );
}

crate::scenario_table!(
    claimed_visual_resolution_regressions,
    negotiated_live_claim_requires_complete_exact_candidate_resolution,
    worker_dispute_labels_cannot_replace_operator_or_review_authority,
    fixed_visual_claim_is_checked_against_real_review_and_recapture
);

fn fixed_visual_claim_is_checked_against_real_review_and_recapture() {
    use super::super::claimed_review::{ReviewedLeaf, ToOwner};
    use orbit_store::contracts::HandoffReviewObservation;
    use orbit_types::workflow::{
        REVIEW_REPORT_ARTIFACT, ReviewReport, ReviewVerdict, ReviewVisualResolution,
    };
    let LiveClaim {
        pair,
        _kit,
        mut handoff,
        mut bundle,
        mut capture,
        mut review,
        ..
    } = LiveClaim::with_config(
        "[review]\nbefore_landing = true\n[operation]\nreview_crew = \"sol\"\n",
    );
    let evidence_worker = ClaimInvocation::trusted_worker(
        handoff.task_id.clone(),
        handoff.claim_id.clone(),
        handoff.machine_id.clone(),
        Some(ClaimRun {
            machine_id: handoff.machine_id.clone(),
            run_id: handoff.run_id.clone(),
        }),
    );
    let sequence = std::cell::Cell::new(0);
    let records = |owner: &OrbitRuntime, artifacts: Vec<TaskArtifact>| {
        let next = sequence.get() + 1;
        sequence.set(next);
        owner
            .mutate_execution_claim(
                Some(&evidence_worker),
                &format!("fixed-visual-{next}"),
                &ClaimMutation::Evidence(ClaimEvidence {
                    artifacts,
                    ..Default::default()
                }),
            )
            .unwrap();
    };
    let leaf_id = handoff.run_id.clone();
    let admission = pair.admission(&leaf_id);
    let drain = admission.request.run_context.run_id.clone();
    let claim = admission.receipt.as_ref().unwrap().claim.clone().unwrap();
    let repo = &pair.follower_repo;
    git(repo, &["init", "-q", "-b", "main"]);
    git(repo, &["config", "core.bare", "false"]);
    std::fs::write(repo.join(".gitignore"), ".orbit/\n").unwrap();
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/f0.rs"), "fn work() {}\n").unwrap();
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "baseline"]);
    git(
        repo,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/owner/repository.git",
        ],
    );
    let base = revision(repo, "HEAD");
    git(
        repo,
        &["checkout", "-q", "-b", &handoff.candidate.source_branch],
    );
    std::fs::write(repo.join("src/f0.rs"), "fn work() { todo!() }\n").unwrap();
    git(repo, &["commit", "-q", "-am", "Implement"]);
    let initial = revision(repo, "HEAD");
    capture["candidate_sha"] = json!(initial.commit);
    capture["mocks"][0]["capture"]["commit"] = json!(initial.commit);
    review["candidate_sha"] = json!(initial.commit);
    review["findings"] = json!([{"finding_id":"f1","source":"comparison","question":"1","mock":"mocks/checkout.png",
        "title":"missing action","detail":"absent","severity":"high","verdict":{"verdict":"confirmed","reasoning":"verified"},
        "disposition":"must_address"}]);
    records(
        &pair.wire.owner,
        vec![
            LiveClaim::artifact("visual/capture.json", &capture),
            LiveClaim::artifact("visual/review.json", &review),
        ],
    );
    let bound = pair
        .follower
        .clone()
        .with_worker_invocation(
            orbit_types::tool::WorkerInvocation {
                owner_machine_id: OWNER.into(),
                owner_workspace_id: admission.destination.owner_workspace_id,
                owner_destination: admission.destination.selector,
                task_id: claim.task_id,
                claim_id: claim.claim_id,
                execution: claim.executed_on,
                bound_run_id: leaf_id.clone(),
            },
            Arc::new(ToOwner(pair.wire.owner.clone())),
        )
        .unwrap();
    let task = handoff.task_id.clone();
    let gate_input = json!({"run_id":leaf_id,"job_run_id":leaf_id,"completed_task_ids":[task],"workspace_path":repo,"base":"main","base_sync":"local","mode":"pr","before_landing":true,"skipped_no_diff_expected":false});
    let mut leaf = ReviewedLeaf {
        pair,
        drain,
        leaf: leaf_id,
        task,
        bound,
        base: base.clone(),
        gate_input,
    };
    let admitted = leaf.admit_review();
    assert_eq!(admitted["applies"], true, "{admitted}");
    let attempt = admitted["attempt_id"].as_str().unwrap();
    leaf.reviewer_reports(attempt, ReviewVerdict::AcceptWithFixes, true);
    let mut report =
        ReviewReport::parse(&leaf.owner_artifact(REVIEW_REPORT_ARTIFACT).unwrap()).unwrap();
    report.visual_resolutions = vec![ReviewVisualResolution {
        finding_id: "f1".into(),
        resolution: "fixed".into(),
        reason: None,
    }];
    leaf.put_report(&report);
    let settled = leaf.settle().unwrap();
    assert_eq!(settled["gate"], "passed", "{settled}");
    let head = revision(&leaf.pair.follower_repo, "HEAD");
    // Give the owner the actual candidate objects, like the published-PR
    // observer's fetch. No external forge is contacted by this fixture.
    git(
        &leaf.pair.owner_repo,
        &[
            "fetch",
            "-q",
            leaf.pair.follower_repo.to_str().unwrap(),
            &handoff.candidate.source_branch,
        ],
    );
    handoff.candidate.candidate = head.clone();
    handoff.candidate.base = base;
    handoff.review = HandoffReview {
        policy: ReviewTiming::BeforeLanding,
        disposition: HandoffReviewDisposition::BeforeLanding(Box::new(
            serde_json::from_value(settled["handoff_evidence"].clone()).unwrap(),
        )),
        selective_receipt: None,
    };
    bundle.candidate_sha = head.commit.clone();
    let initial_artifact = LiveClaim::artifact("visual/capture.json", &capture);
    bundle.initial_capture.sha256 = mock_sha256_hex(&initial_artifact.content);
    capture["candidate_sha"] = json!(head.commit);
    capture["mocks"][0]["capture"]["commit"] = json!(head.commit);
    let final_artifact = LiveClaim::artifact("visual/recapture/capture.json", &capture);
    bundle.final_capture = HandoffArtifactRef {
        path: final_artifact.path.clone(),
        sha256: mock_sha256_hex(&final_artifact.content),
    };
    review["recapture"] =
        json!({"candidate_sha":head.commit,"outcome":"passed","still_failing":[]});
    review["findings"][0]["resolution"] = json!("fixed");
    let review_artifact = LiveClaim::artifact("visual/review.json", &review);
    bundle.review.sha256 = mock_sha256_hex(&review_artifact.content);
    let checks = LiveClaim::artifact(
        "visual/recapture/checks.json",
        &json!({"schema_version":1,"mode":"live","candidate_sha":head.commit,"blocking":false,"findings":[]}),
    );
    bundle.final_checks = HandoffArtifactRef {
        path: checks.path.clone(),
        sha256: mock_sha256_hex(&checks.content),
    };
    let proof = LiveClaim::artifact(VISUAL_RESOLUTION_ARTIFACT, &bundle);
    handoff.visual = Some(HandoffArtifactRef {
        path: proof.path.clone(),
        sha256: mock_sha256_hex(&proof.content),
    });
    let owner = &leaf.pair.wire.owner;
    records(
        owner,
        vec![
            initial_artifact,
            final_artifact,
            checks,
            review_artifact,
            proof,
        ],
    );
    let observation = HandoffObservation {
        candidate: handoff.candidate.clone(),
        footprint_widening: vec![],
        required_commands: vec![],
        owner_completion_authority: None,
        review: Some(HandoffReviewObservation {
            reviewed_base_sha: handoff.candidate.base.commit.clone(),
            reviewed_base_is_ancestor: true,
            repository: "owner/repository".into(),
        }),
        known_red: vec![],
        light_delivery: vec![],
    };
    let (_, worker) = leaf.claim();
    // A worker's passed recapture label cannot hide a missing rendered mock.
    capture["mocks"][0]["capture"] = Value::Null;
    let partial = LiveClaim::artifact("visual/recapture/capture.json", &capture);
    bundle.final_capture.sha256 = mock_sha256_hex(&partial.content);
    let partial_proof = LiveClaim::artifact(VISUAL_RESOLUTION_ARTIFACT, &bundle);
    let valid_ref = handoff.visual.clone();
    handoff.visual.as_mut().unwrap().sha256 = mock_sha256_hex(&partial_proof.content);
    records(owner, vec![partial, partial_proof]);
    let error = owner
        .accept_task_handoff(&worker, "partial", handoff.clone(), observation.clone())
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("delivery_visual_review_unresolved"),
        "{error}"
    );
    // Restore the complete exact-head evidence and the same real certificate.
    capture["mocks"][0]["capture"] = json!({"path":"visual/checkout/capture.png","side_by_side":"visual/checkout/side-by-side.png",
        "side_by_side_sha256":mock_sha256_hex(SIDE_BY_SIDE),"commit":head.commit,"viewport":[1440,900],"device_scale_factor":1,"fixture":"default","fixture_sha256":"0".repeat(64),
        "captured_at":"2026-10-29T00:00:00Z","sha256":mock_sha256_hex(CAPTURE),"stability":"stable","encoding":"png","lossy":false});
    let restored = LiveClaim::artifact("visual/recapture/capture.json", &capture);
    bundle.final_capture.sha256 = mock_sha256_hex(&restored.content);
    let proof = LiveClaim::artifact(VISUAL_RESOLUTION_ARTIFACT, &bundle);
    handoff.visual = valid_ref;
    records(owner, vec![restored, proof]);
    owner
        .accept_task_handoff(&worker, "complete", handoff.clone(), observation)
        .expect("real fixed finding with complete final recapture accepted");
    let accepted = owner.accepted_task_handoff(&handoff.claim_id).unwrap();
    RuntimeHost::handoff_landing_context(owner, &accepted.handoff_id).unwrap();
}
