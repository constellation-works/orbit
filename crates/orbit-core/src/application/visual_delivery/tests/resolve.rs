//! `visual_recapture` over a live delivery's blocking finding: a held
//! finding needs a reviewer the review gate actually issued it to. When the
//! gate ran none (selective `not_required`, a review-skip tag), nothing can
//! fix or dispute the finding, so it is filed as a `high` follow-up rather
//! than held where `orbit visual accept-dispute` refuses to release it.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use chrono::Utc;
use orbit_engine::DispatchError;
use orbit_types::task::{
    TaskArtifact, VisualCaptureImage, VisualCaptureOutcome, VisualCaptureRecord,
    VisualCapturedMock, VisualDisposition, VisualFindingVerdict, VisualPass,
    VisualReviewBudgetRecord, VisualReviewFinding, VisualReviewMode, VisualReviewRecord,
    VisualVerdictKind, mock_sha256_hex,
};
use orbit_types::workflow::automation::SourceRevision;
use orbit_types::workflow::{
    DeliveryRefusal, REVIEW_MANIFEST_ARTIFACT, ReviewBudget, ReviewConsumption, ReviewManifest,
    ReviewVisualFinding,
};
use serde_json::json;

use super::super::visual_recapture_step;
use crate::OrbitRuntime;
use crate::application::task::{TaskAddParams, TaskUpdateParams};
use crate::application::test_support::runtime_with_workspace_config;

const CONFIG: &str = "[visual_review]\nmode = \"live\"\nmanifests = [\"views.toml\"]\n";
const VIEWS: &str = "schema_version = 1\n\n[app.web]\nthemes = [\"light\"]\n\n\
                     [[view]]\nid = \"checkout\"\napp = \"web\"\n";
const MOCK_NAME: &str = "mocks/checkout.light.png";
const MOCK: &[u8] = b"\x89PNG\r\n\x1a\nmock";
const FINDING: &str = "f1";

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.com",
        ])
        .args(args)
        .current_dir(repo)
        .output()
        .expect("run git");
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8(output.stdout)
        .expect("utf8")
        .trim()
        .to_string()
}

fn json_artifact(path: &str, record: &impl serde::Serialize) -> TaskArtifact {
    TaskArtifact {
        path: path.to_string(),
        content: serde_json::to_vec(record).expect("encode record"),
        media_type: "application/json".to_string(),
        created_by: None,
    }
}

fn blocking_finding() -> VisualReviewFinding {
    VisualReviewFinding {
        finding_id: FINDING.to_string(),
        source: "comparison".to_string(),
        question: "1".to_string(),
        mock: MOCK_NAME.to_string(),
        title: "The checkout total is missing".to_string(),
        detail: "The mock shows a total the capture lacks.".to_string(),
        evidence: Vec::new(),
        severity: "high".to_string(),
        verified_severity: Some("high".to_string()),
        severity_source: Some("verified".to_string()),
        verdict: VisualFindingVerdict {
            verdict: VisualVerdictKind::Confirmed,
            crew: Some("verifier".to_string()),
            confidence: Some("high".to_string()),
            severity: Some("high".to_string()),
            escalated: false,
            reasoning: "confirmed".to_string(),
            deviation_rejected: false,
            rejection_reason: None,
        },
        disposition: Some(VisualDisposition::MustAddress),
        resolution: None,
        resolution_reason: None,
        recaptured: Vec::new(),
        follow_up: None,
    }
}

/// A live `verify-mock-ui` delivery of one task whose candidate is the
/// repository head, with one confirmed blocking finding settled as
/// `must_address`. Returns the runtime, task id and the repository.
fn held_delivery() -> (tempfile::TempDir, OrbitRuntime, String, std::path::PathBuf) {
    let (root, runtime, repo) = runtime_with_workspace_config(Some(CONFIG));
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "core.bare", "false"]);
    std::fs::write(repo.join(".gitignore"), ".orbit/\n").expect("gitignore");
    std::fs::write(repo.join("views.toml"), VIEWS).expect("views");
    git(&repo, &["add", ".gitignore", "views.toml"]);
    git(&repo, &["commit", "-q", "-m", "views"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);

    let task = runtime
        .add_task(TaskAddParams {
            title: "Checkout view".to_string(),
            description: "Fixture task for visual resolution.".to_string(),
            acceptance_criteria: vec!["The view matches its mock.".to_string()],
            plan: "Build the view.".to_string(),
            tags: vec!["verify-mock-ui".to_string()],
            ..TaskAddParams::default()
        })
        .expect("seed task");
    let mock_sha = mock_sha256_hex(MOCK);
    let image = VisualCaptureImage {
        path: "visual/checkout/capture.png".to_string(),
        side_by_side: "visual/checkout/side-by-side.png".to_string(),
        side_by_side_sha256: "0".repeat(64),
        commit: head.clone(),
        viewport: [1440, 900],
        device_scale_factor: 1,
        fixture: "default".to_string(),
        fixture_sha256: "0".repeat(64),
        captured_at: Utc::now().to_rfc3339(),
        sha256: "0".repeat(64),
        stability: "stable".to_string(),
        diff_region: None,
        encoding: "png".to_string(),
        lossy: false,
        off_loopback_requests: Vec::new(),
        color_scheme_leak: None,
    };
    let capture = VisualCaptureRecord {
        schema_version: 1,
        outcome: VisualCaptureOutcome::Captured,
        mode: VisualReviewMode::Live,
        task_id: task.id.clone(),
        host: "fixture".to_string(),
        kit: None,
        candidate_sha: head.clone(),
        manifests: Vec::new(),
        frozen_instant: None,
        mocks: vec![VisualCapturedMock {
            artifact: MOCK_NAME.to_string(),
            sha256: mock_sha,
            view: "checkout".to_string(),
            state: "default".to_string(),
            theme: "light".to_string(),
            width: 1440,
            capture: Some(image),
            error: None,
        }],
        over_budget: Vec::new(),
        not_established: Vec::new(),
    };
    let review = VisualReviewRecord {
        schema_version: 1,
        purpose: "visual_review".to_string(),
        mode: VisualReviewMode::Live,
        task_id: task.id.clone(),
        candidate_sha: head,
        recorded_at: (Utc::now() - chrono::Duration::minutes(1)).to_rfc3339(),
        outcome: "reviewed".to_string(),
        not_established: Vec::new(),
        reviewer_crew: Some("reviewer".to_string()),
        verifier_crews: vec!["verifier".to_string()],
        budget: VisualReviewBudgetRecord {
            max_mocks: 12,
            max_images: 32,
            max_image_tokens: 48_000,
            max_findings: 15,
            images_shown: 2,
            estimated_image_tokens: 1_000,
            shown: Vec::new(),
            not_shown: Vec::new(),
        },
        findings: vec![blocking_finding()],
        invalid_evidence_dropped: 0,
        over_max_findings_dropped: 0,
        deviations: Vec::new(),
        recorded_only: false,
        block_at: None,
        file_at: None,
        recapture: None,
    };
    runtime
        .update_task_as_system(
            &task.id,
            TaskUpdateParams {
                upsert_artifacts: vec![
                    TaskArtifact {
                        path: MOCK_NAME.to_string(),
                        content: MOCK.to_vec(),
                        media_type: "image/png".to_string(),
                        created_by: None,
                    },
                    json_artifact(&VisualPass::Delivery.capture_artifact(), &capture),
                    json_artifact(&VisualPass::Delivery.review_artifact(), &review),
                ],
                ..Default::default()
            },
            None,
        )
        .expect("seed visual evidence");
    (root, runtime, task.id, repo)
}

/// The manifest of a reviewer admitted for the delivery, issued with the
/// blocking finding in front of it.
fn issue_reviewer_manifest(runtime: &OrbitRuntime, task_id: &str) {
    let revision = SourceRevision {
        commit: "1".repeat(40),
        tree: "2".repeat(40),
    };
    let manifest = ReviewManifest {
        satisfied_external_evidence: BTreeMap::new(),
        evidence_carried: None,
        owed_external_evidence: Vec::new(),
        owner_owed_evidence: Vec::new(),
        host_validation: Vec::new(),
        mutation_proposals: Vec::new(),
        visual_findings: vec![ReviewVisualFinding {
            finding_id: FINDING.to_string(),
            mock: MOCK_NAME.to_string(),
            question: "1".to_string(),
            title: "The checkout total is missing".to_string(),
            detail: "The mock shows a total the capture lacks.".to_string(),
            severity: "high".to_string(),
            must_address: true,
            images: Vec::new(),
        }],
        previous_report: None,
        schema_version: 1,
        attempt_id: "attempt-1".to_string(),
        lineage_key: "lineage".to_string(),
        task_ids: vec![task_id.to_string()],
        task_digests: BTreeMap::new(),
        required_validation_commands: None,
        baseline_commands: Vec::new(),
        task_meaning_digest: "digest".to_string(),
        repository: "repo".to_string(),
        base: revision.clone(),
        candidate: revision,
        implementation_commits: Vec::new(),
        implementer_summaries: BTreeMap::new(),
        reviewer_crew: "reviewer".to_string(),
        contract_version: 1,
        policy_version: 1,
        budget: ReviewBudget::default(),
        remaining: ReviewConsumption { seconds: 600 },
        issued_at: Utc::now(),
    };
    runtime
        .update_task_as_system(
            task_id,
            TaskUpdateParams {
                upsert_artifacts: vec![json_artifact(REVIEW_MANIFEST_ARTIFACT, &manifest)],
                ..Default::default()
            },
            None,
        )
        .expect("seed reviewer manifest");
}

fn recapture(
    runtime: &OrbitRuntime,
    task_id: &str,
    repo: &Path,
) -> Result<serde_json::Value, DispatchError> {
    visual_recapture_step(
        runtime,
        "visual_recapture",
        &json!({
            "workspace_path": repo.display().to_string(),
            "completed_task_ids": [task_id],
            "job_run_id": "jrun-fixture",
        }),
    )
}

fn stored_finding(runtime: &OrbitRuntime, task_id: &str) -> VisualReviewFinding {
    let stored = runtime
        .get_task_artifact(task_id, &VisualPass::Delivery.review_artifact())
        .expect("read review")
        .expect("review artifact");
    let record: VisualReviewRecord =
        serde_json::from_slice(&stored.content).expect("review record");
    record.findings.into_iter().next().expect("finding")
}

#[test]
fn blocking_finding_with_no_reviewer_issued_is_a_high_follow_up_not_a_hold() {
    let (_root, runtime, task_id, repo) = held_delivery();

    let output =
        recapture(&runtime, &task_id, &repo).expect("a finding no reviewer was issued is not held");

    assert_eq!(output["applies"], json!(true), "{output}");
    let finding = stored_finding(&runtime, &task_id);
    assert_eq!(finding.disposition, Some(VisualDisposition::FollowUpHigh));
    assert!(finding.files_follow_up(), "{finding:?}");
}

#[test]
fn blocking_finding_given_to_a_reviewer_that_did_not_resolve_it_still_holds() {
    let (_root, runtime, task_id, repo) = held_delivery();
    issue_reviewer_manifest(&runtime, &task_id);

    let error = recapture(&runtime, &task_id, &repo).expect_err("an unresolved finding holds");

    assert!(
        matches!(
            error,
            DispatchError::DeliveryRefused {
                refusal: DeliveryRefusal::VisualReviewUnresolved,
                ..
            }
        ),
        "{error:?}"
    );
    let finding = stored_finding(&runtime, &task_id);
    assert_eq!(finding.disposition, Some(VisualDisposition::MustAddress));
}
