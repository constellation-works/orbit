//! The checks step over a capture whose every mock is over the image budget.
//! The capture is not established, but the driver's UI rule results in
//! `rules.json` still reach `visual/checks.json` (§7), beside the
//! `image_over_budget` findings and the diagnostics the capture step kept.

use std::collections::BTreeMap;

use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use orbit_types::task::{
    TaskArtifact, VISUAL_CAPTURE_ARTIFACT, VISUAL_CHECKS_ARTIFACT, VisualCaptureError,
    VisualCaptureOutcome, VisualCaptureRecord, VisualCapturedMock, VisualCheckFinding,
    VisualChecksRecord, VisualNotEstablished, VisualReviewMode,
};
use serde_json::json;

use crate::application::task::{TaskAddParams, TaskUpdateParams};
use crate::application::test_support::runtime_with_workspace_layout;

const CANDIDATE: &str = "1111111111111111111111111111111111111111";
const LIGHT: &str = "mocks/checkout-light.png";
const DARK: &str = "mocks/checkout-dark.png";

fn over_budget(artifact: &str, theme: &str) -> VisualCapturedMock {
    VisualCapturedMock {
        artifact: artifact.to_string(),
        sha256: "0".repeat(64),
        view: "checkout".to_string(),
        state: "default".to_string(),
        theme: theme.to_string(),
        width: 1440,
        capture: None,
        error: Some(VisualCaptureError {
            code: "image_over_budget".to_string(),
            message: format!("{artifact} remains over the artifact limit after WebP encoding"),
        }),
    }
}

fn json_artifact(path: &str, record: &impl serde::Serialize) -> TaskArtifact {
    TaskArtifact {
        path: path.to_string(),
        content: serde_json::to_vec(record).expect("encode record"),
        media_type: "application/json".to_string(),
        created_by: None,
    }
}

#[test]
fn all_over_budget_capture_keeps_ui_rule_results() {
    let (root, runtime, _repo) = runtime_with_workspace_layout();
    let task = runtime
        .add_task(TaskAddParams {
            title: "Every mock is over the image budget".to_string(),
            description: "Fixture task for the visual checks step.".to_string(),
            acceptance_criteria: vec!["The checks keep the UI rule results.".to_string()],
            plan: "Run the checks over the capture.".to_string(),
            ..TaskAddParams::default()
        })
        .expect("seed task");
    let capture = VisualCaptureRecord {
        schema_version: 1,
        outcome: VisualCaptureOutcome::NotEstablished,
        mode: VisualReviewMode::Shadow,
        task_id: task.id.clone(),
        host: "fixture".to_string(),
        kit: None,
        candidate_sha: CANDIDATE.to_string(),
        manifests: Vec::new(),
        frozen_instant: None,
        mocks: vec![over_budget(LIGHT, "light"), over_budget(DARK, "dark")],
        over_budget: Vec::new(),
        not_established: vec![VisualNotEstablished {
            reason: "capture_failed".to_string(),
            detail: "no mock rendered a capture".to_string(),
        }],
    };
    // The capture step kept the off-loopback request it saw before the image budget failed.
    let preserved = VisualChecksRecord {
        schema_version: 1,
        mode: VisualReviewMode::Shadow,
        candidate_sha: CANDIDATE.to_string(),
        blocking: false,
        findings: vec![VisualCheckFinding {
            check: "check:off_loopback_request".to_string(),
            mock: Some(LIGHT.to_string()),
            severity: "medium".to_string(),
            title: "The view requested an address off loopback".to_string(),
            detail: "https://external.invalid/data".to_string(),
            against: "change".to_string(),
            region: None,
            measurements: None,
        }],
        rule_results: BTreeMap::new(),
        rules_unavailable: None,
    };
    runtime
        .update_task_as_system(
            &task.id,
            TaskUpdateParams {
                upsert_artifacts: vec![
                    json_artifact(VISUAL_CAPTURE_ARTIFACT, &capture),
                    json_artifact(VISUAL_CHECKS_ARTIFACT, &preserved),
                ],
                ..Default::default()
            },
            None,
        )
        .expect("seed the capture record");

    // The driver ran both mocks' rules before the image budget failed, and wrote them to rules.json.
    let scratch = root.path().join("scratch");
    let out = scratch.join("out");
    std::fs::create_dir_all(&out).expect("scratch output");
    let rules = json!({
        "schema_version": 1,
        "source": "scripts/ux-rules/ui",
        "results": [
            {
                "rule": "ui.contrast", "mock": LIGHT, "status": "fail", "severity": "high",
                "title": "Text contrast is below the minimum", "selector": ".price",
                "measurements": {"ratio": 2.1}, "fingerprint": "light-contrast"
            },
            {
                "rule": "ui.contrast", "mock": DARK, "status": "pass", "severity": "high",
                "title": "Text contrast is below the minimum", "selector": "",
                "fingerprint": "dark-contrast"
            },
            {
                "rule": "ui.touch-target", "mock": DARK, "status": "skipped", "severity": "low",
                "title": "Touch targets are too small", "selector": "",
                "fingerprint": "dark-touch"
            },
        ],
    });
    std::fs::write(
        out.join("rules.json"),
        serde_json::to_vec_pretty(&rules).expect("encode rules"),
    )
    .expect("write rules.json");

    let input = json!({
        "capture": {"captures": [{
            "task_id": task.id.as_str(),
            "scratch_dir": scratch.display().to_string(),
        }]},
    });
    runtime
        .run_deterministic("visual_checks", &json!({}), &input, ToolContext::default())
        .expect("visual_checks runs");

    let stored = runtime
        .get_task_artifact(&task.id, VISUAL_CHECKS_ARTIFACT)
        .expect("read checks")
        .expect("checks artifact");
    let checks: VisualChecksRecord =
        serde_json::from_slice(&stored.content).expect("checks record");
    let has = |check: &str, mock: &str| {
        checks
            .findings
            .iter()
            .any(|finding| finding.check == check && finding.mock.as_deref() == Some(mock))
    };
    assert!(
        has("check:image_over_budget", LIGHT),
        "{:?}",
        checks.findings
    );
    assert!(
        has("check:image_over_budget", DARK),
        "{:?}",
        checks.findings
    );
    assert!(
        has("check:off_loopback_request", LIGHT),
        "{:?}",
        checks.findings
    );
    assert!(has("rule:ui.contrast", LIGHT), "{:?}", checks.findings);
    assert!(
        !has("rule:ui.contrast", DARK),
        "a passing rule is not a finding: {:?}",
        checks.findings
    );
    assert_eq!(checks.findings.len(), 4, "{:?}", checks.findings);
    assert_eq!(
        checks.rule_results,
        BTreeMap::from([
            ("fail".to_string(), 1),
            ("pass".to_string(), 1),
            ("skipped".to_string(), 1),
        ]),
    );
    assert_eq!(checks.rules_unavailable, None);
}
