//! A claimed `verify-mock-ui` leaf captures against the visual review
//! contract its owner captured at admission and returns the capture in its
//! handoff. The owner refuses a handoff whose capture does not match that
//! contract with `visual_capture_mismatch` (visual-review §6.2).

use orbit_core::bootstrap::browser_kit::PLAYWRIGHT_VERSION;
use orbit_types::task::{
    HostOs, VisualCaptureImage, VisualCaptureOutcome, VisualCaptureRecord, VisualCapturedMock,
    VisualKitPin, VisualNotEstablished, VisualReviewContract, VisualSettleOutcome,
    VisualSettleRecord, VisualSettleRefusal, mock_sha256_hex,
};

use super::*;

const CONFIG: &str = "[visual_review]\nmode = \"shadow\"\nmanifests = [\"views.toml\"]\n";
const MANIFEST: &str = "schema_version = 1\n\n[app.web]\nthemes = [\"light\", \"dark\"]\n\n\
                        [[view]]\nid = \"checkout\"\napp = \"web\"\n";
const MOCK: &[u8] = b"\x89PNG\r\n\x1a\nmock";
const CAPTURE: &[u8] = b"\x89PNG\r\n\x1a\ncapture";
const SIDE_BY_SIDE: &[u8] = b"\x89PNG\r\n\x1a\nside-by-side";

/// How the leaf's returned capture departs from the contract, if at all.
#[derive(Clone, Copy, PartialEq)]
enum Returned {
    Matching,
    /// The stored capture image is not the bytes `capture.json` records.
    ForgedImage,
    /// A capture of a commit other than the handed-off head.
    OtherCommit,
    /// No capture at all.
    Nothing,
}

/// An owner whose one backlog task is `verify-mock-ui` with a valid mock
/// against a committed view manifest, in shadow mode, and a follower with a
/// verified kit that claims it. Returns the pair, its drain and leaf, and
/// the contract the claim's receipt carries.
fn claimed_mock_task() -> (Pair, TempDir, String, String, VisualReviewContract) {
    let mut pair = Pair::with_owner_config(CONFIG, &[None]);
    let repo = pair.owner_repo.clone();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "core.bare", "false"]);
    std::fs::write(repo.join(".gitignore"), ".orbit/\n").unwrap();
    std::fs::write(repo.join("views.toml"), MANIFEST).unwrap();
    git(&repo, &["add", ".gitignore", "views.toml"]);
    git(&repo, &["commit", "-q", "-m", "views"]);
    let owner = pair.wire.owner.clone();
    let task = pair.tasks[0].clone();
    let source = repo.join(".orbit/mock-fixture.png");
    std::fs::write(&source, MOCK).unwrap();
    // Mocks are the operator's: an agent cannot replace its own specification.
    owner
        .run_tool_with_context_and_role(
            "orbit.task.artifact.put",
            json!({
                "id": task,
                "path": "mocks/checkout.png",
                "source_path": source.to_string_lossy(),
            }),
            Role::Admin,
            ToolContext {
                session_context: ToolSessionContext {
                    effective_capabilities: BTreeSet::from([McpCapability::Operator]),
                    ..ToolSessionContext::default()
                },
                ..ToolContext::default()
            },
        )
        .expect("attach mock");
    owner
        .run_tool(
            "orbit.task.update",
            json!({"id": task, "tags": ["verify-mock-ui"], "model": "codex"}),
        )
        .expect("tag verify-mock-ui");
    let kit_home = TempDir::new_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    super::super::host_ui_routing::write_kit(kit_home.path(), true);
    pair.follower = pair
        .follower
        .clone()
        .with_host_os(Some(HostOs::Linux))
        .with_browser_kit_home(kit_home.path());
    *pair.wire.accept_handoffs.lock().unwrap() = true;
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    assert_eq!(pair.claimed_task(&leaf), task);
    let contract = pair
        .admission(&leaf)
        .receipt
        .and_then(|receipt| receipt.visual_review)
        .expect("the claim's receipt carries the owner's visual review contract");
    (pair, kit_home, drain, leaf, contract)
}

/// Hand the leaf off returning `returned`, and the owner's settlement pass.
fn hand_off(returned: Returned) -> (Pair, Value, String) {
    let (pair, _kit, drain, leaf, contract) = claimed_mock_task();
    let record = pair.admission(&leaf);
    let mut delivered = handoff(&record);
    if returned != Returned::Nothing {
        let head = delivered.candidate.candidate.commit.clone();
        let commit = if returned == Returned::OtherCommit {
            "f".repeat(40)
        } else {
            head.clone()
        };
        let capture = VisualCaptureRecord {
            schema_version: 1,
            outcome: VisualCaptureOutcome::Captured,
            mode: contract.mode,
            task_id: contract.task_id.clone(),
            host: FOLLOWER.into(),
            kit: Some(VisualKitPin {
                playwright: PLAYWRIGHT_VERSION.into(),
                browser_version: None,
                pixel_helper: None,
                verified_at: None,
            }),
            candidate_sha: head,
            manifests: contract
                .manifests
                .iter()
                .map(|manifest| manifest.reference.clone())
                .collect(),
            frozen_instant: None,
            mocks: contract
                .mocks
                .iter()
                .map(|mock| VisualCapturedMock {
                    artifact: mock.artifact.clone(),
                    sha256: mock.sha256.clone(),
                    view: mock.view.clone(),
                    state: mock.state.clone(),
                    theme: mock.theme.clone(),
                    width: mock.width,
                    capture: Some(VisualCaptureImage {
                        path: "visual/checkout/capture.png".into(),
                        side_by_side: "visual/checkout/side-by-side.png".into(),
                        side_by_side_sha256: mock_sha256_hex(SIDE_BY_SIDE),
                        commit: commit.clone(),
                        viewport: [mock.width, 900],
                        device_scale_factor: 1,
                        fixture: "default".into(),
                        fixture_sha256: "0".repeat(64),
                        captured_at: "2026-10-29T00:00:00Z".into(),
                        sha256: mock_sha256_hex(CAPTURE),
                        stability: "stable".into(),
                        diff_region: None,
                        encoding: "png".into(),
                        lossy: false,
                        off_loopback_requests: Vec::new(),
                        color_scheme_leak: None,
                    }),
                    error: None,
                })
                .collect(),
            over_budget: Vec::new(),
            not_established: Vec::new(),
        };
        let capture_json = serde_json::to_vec(&capture).unwrap();
        let stored_capture = if returned == Returned::ForgedImage {
            b"\x89PNG\r\n\x1a\nrelabelled".to_vec()
        } else {
            CAPTURE.to_vec()
        };
        let artifact = |path: &str, content: Vec<u8>, media_type: &str| TaskArtifact {
            path: path.into(),
            content,
            media_type: media_type.into(),
            created_by: None,
        };
        let context = ClaimInvocation::trusted_worker(
            delivered.task_id.clone(),
            delivered.claim_id.clone(),
            delivered.machine_id.clone(),
            Some(ClaimRun {
                machine_id: delivered.machine_id.clone(),
                run_id: delivered.run_id.clone(),
            }),
        );
        pair.wire
            .owner
            .mutate_execution_claim(
                Some(&context),
                "visual-capture",
                &ClaimMutation::Evidence(ClaimEvidence {
                    artifacts: vec![
                        artifact("visual/checkout/capture.png", stored_capture, "image/png"),
                        artifact(
                            "visual/checkout/side-by-side.png",
                            SIDE_BY_SIDE.to_vec(),
                            "image/png",
                        ),
                        artifact(
                            "visual/capture.json",
                            capture_json.clone(),
                            "application/json",
                        ),
                    ],
                    ..Default::default()
                }),
            )
            .expect("the leaf's capture reaches the owner");
        delivered.visual = Some(HandoffArtifactRef {
            path: "visual/capture.json".into(),
            sha256: sha256_hex(&capture_json),
        });
    }
    pair.advance(
        &leaf,
        LocalPullMutation::Settle(Box::new(ClaimMutation::AcceptHandoff(delivered))),
    );
    pair.follower_jobs
        .finalize_job_run(&leaf, JobRunState::Success, Utc::now(), None)
        .unwrap();
    let settled = pair.pass(&drain);
    (pair, settled, drain)
}

fn a_claimed_capture_matching_its_contract_is_accepted() {
    let (pair, settled, _) = hand_off(Returned::Matching);
    assert!(settled["error"].is_null(), "{settled}");
    assert_eq!(pair.owner_status(&pair.tasks[0]), "review");
}

fn a_claimed_capture_that_departs_from_its_contract_is_refused() {
    for (returned, detail) in [
        (Returned::ForgedImage, "digest differs from capture.json"),
        (Returned::OtherCommit, "not the handed-off candidate"),
        (Returned::Nothing, "returns no capture"),
    ] {
        let (pair, settled, drain) = hand_off(returned);
        assert_eq!(settled["settlement_refused"], 1, "{settled}");
        let refused = pair
            .follower
            .pull_drain_refused_settlements(&drain)
            .unwrap();
        assert!(
            refused[0].reason.contains("visual_capture_mismatch")
                && refused[0].reason.contains(detail),
            "the owner never accepts a capture its claim's contract does not pin: {refused:?}"
        );
        assert_ne!(pair.owner_status(&pair.tasks[0]), "review");
    }
}

/// Task show reads the capture of record alone when the stored settle was
/// made for another candidate: a stale `Refused` settle (held) must neither
/// replace the summary nor surface as the task's settle, mirroring the
/// mismatched-checks filter [ORB-20109].
fn task_show_ignores_a_settle_for_another_candidate() {
    let (pair, _kit, _drain, leaf, contract) = claimed_mock_task();
    let record = pair.admission(&leaf);
    let delivered = handoff(&record);
    let task = delivered.task_id.clone();
    let capture_sha = "a".repeat(40);
    let stale_sha = "b".repeat(40);
    let frozen = contract
        .mocks
        .first()
        .expect("the contract freezes one mock");
    let capture = VisualCaptureRecord {
        schema_version: 1,
        outcome: VisualCaptureOutcome::NotEstablished,
        mode: contract.mode,
        task_id: task.clone(),
        host: FOLLOWER.into(),
        kit: None,
        candidate_sha: capture_sha.clone(),
        manifests: contract
            .manifests
            .iter()
            .map(|manifest| manifest.reference.clone())
            .collect(),
        frozen_instant: None,
        mocks: vec![VisualCapturedMock {
            artifact: frozen.artifact.clone(),
            sha256: frozen.sha256.clone(),
            view: frozen.view.clone(),
            state: frozen.state.clone(),
            theme: frozen.theme.clone(),
            width: frozen.width,
            capture: None,
            error: None,
        }],
        over_budget: Vec::new(),
        not_established: vec![VisualNotEstablished {
            reason: "browser_kit_missing".into(),
            detail: "the kit is missing".into(),
        }],
    };
    let settle = VisualSettleRecord {
        schema_version: 1,
        candidate_sha: stale_sha,
        outcome: VisualSettleOutcome::Refused,
        refusals: vec![VisualSettleRefusal {
            code: "stale_capture".into(),
            mock: None,
            detail: "the capture is of another candidate".into(),
        }],
        not_established: Vec::new(),
        held: true,
    };
    let artifact = |path: &str, content: Vec<u8>| TaskArtifact {
        path: path.into(),
        content,
        media_type: "application/json".into(),
        created_by: None,
    };
    let context = ClaimInvocation::trusted_worker(
        delivered.task_id.clone(),
        delivered.claim_id.clone(),
        delivered.machine_id.clone(),
        Some(ClaimRun {
            machine_id: delivered.machine_id.clone(),
            run_id: delivered.run_id.clone(),
        }),
    );
    for (mutation, path, value) in [
        (
            "visual-capture",
            "visual/capture.json",
            serde_json::to_vec(&capture).unwrap(),
        ),
        (
            "visual-settle",
            "visual/settle.json",
            serde_json::to_vec(&settle).unwrap(),
        ),
    ] {
        pair.wire
            .owner
            .mutate_execution_claim(
                Some(&context),
                mutation,
                &ClaimMutation::Evidence(ClaimEvidence {
                    artifacts: vec![artifact(path, value)],
                    ..Default::default()
                }),
            )
            .expect("the visual record reaches the owner");
    }
    let shown = pair.owner_task(&task);
    let visual = &shown["visual_review"];
    assert_eq!(
        visual["candidate_sha"],
        serde_json::json!(capture_sha),
        "task show answers the capture's candidate: {shown}"
    );
    assert_eq!(
        visual["summary"],
        serde_json::json!(format!(
            "{}: not established: browser_kit_missing",
            contract.mode.as_str()
        )),
        "a stale refused, held settle never replaces the capture's summary: {shown}"
    );
    assert!(
        visual.get("settle").is_none_or(Value::is_null),
        "a settle for another candidate never surfaces: {shown}"
    );
}

/// A verdict written before `rejection_reason` was carried still
/// deserializes, with no ground (so it cannot mark a deviation accepted).
#[test]
fn a_verdict_without_rejection_reason_still_deserializes() {
    let verdict: orbit_types::task::VisualFindingVerdict = serde_json::from_value(json!({
        "verdict": "rejected",
        "reasoning": "old record",
        "deviation_rejected": false,
    }))
    .expect("verdict without rejection_reason deserializes");
    assert_eq!(verdict.rejection_reason, None);
    let with_reason: orbit_types::task::VisualFindingVerdict = serde_json::from_value(json!({
        "verdict": "rejected",
        "reasoning": "basis holds",
        "deviation_rejected": false,
        "rejection_reason": "premise_wrong",
    }))
    .expect("verdict with rejection_reason deserializes");
    assert_eq!(
        with_reason.rejection_reason.as_deref(),
        Some("premise_wrong")
    );
}

crate::scenario_table!(
    a_claimed_capture_is_checked_against_its_contract_scenarios,
    a_claimed_capture_matching_its_contract_is_accepted,
    a_claimed_capture_that_departs_from_its_contract_is_refused,
    task_show_ignores_a_settle_for_another_candidate
);
