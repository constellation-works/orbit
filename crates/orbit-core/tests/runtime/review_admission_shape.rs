//! The review gate's admission output is a persisted contract: run state
//! checkpoints it, job templates read its fields, and settlement receives it
//! whole. Its recorded bytes stay the same, and an output recorded before
//! its later fields existed still settles.

use std::collections::BTreeMap;

use chrono::Utc;
use orbit_engine::{DispatchError, RuntimeHost};
use orbit_types::workflow::{
    REVIEW_MANIFEST_ARTIFACT, REVIEW_REPORT_ARTIFACT, ReviewBudget, ReviewConsumption,
    ReviewGateAdmissionOutput, ReviewGateAdmitted, ReviewGateDecision, ReviewGateReviewer,
    ReviewTiming,
};
use serde_json::{Value, json};

use super::review_gate_audit::Fixture;

/// A recorded admitted review gate's output for attempt `attempt_id` in
/// lineage `lineage`, on candidate `head_sha`, reviewed by `codex /
/// review-model`. A fixture that needs another reviewer overwrites
/// `reviewer`.
pub(crate) fn admitted_review(attempt_id: &str, lineage: &str, head_sha: &str) -> Value {
    serde_json::to_value(ReviewGateAdmissionOutput::Admitted(Box::new(
        ReviewGateAdmitted {
            decision: ReviewGateDecision::Admitted,
            held_attempt_id: None,
            first_task_id: "T-1".into(),
            attempt_id: attempt_id.into(),
            attempt_index: 1,
            lineage_key: lineage.into(),
            timing: ReviewTiming::BeforePr,
            timing_source: "workspace".into(),
            reviewer: ReviewGateReviewer {
                crew: "reviewers".into(),
                crew_source: "workspace".into(),
                provider: "codex".into(),
                model: "review-model".into(),
                reasoning_effort: None,
            },
            owed_evidence: Vec::new(),
            owner_owed_evidence: Vec::new(),
            host_validation: Vec::new(),
            base_sha: "0".repeat(40),
            base_tree: "0".repeat(40),
            head_sha: head_sha.into(),
            head_tree: "0".repeat(40),
            implementation_commit_count: 1,
            task_meaning_digest: "digest".into(),
            task_selectors: BTreeMap::new(),
            evidence_carry: Value::Null,
            manifest_artifact: REVIEW_MANIFEST_ARTIFACT.into(),
            report_artifact: REVIEW_REPORT_ARTIFACT.into(),
            budget: ReviewBudget::default(),
            remaining: ReviewConsumption { seconds: 1800 },
            started_at: Utc::now(),
        },
    )))
    .unwrap()
}

/// Serialized output, so field order and explicit `null`s are compared too.
fn bytes(value: &Value) -> String {
    serde_json::to_string(value).unwrap()
}

fn settle_with(fixture: &Fixture, admission: Value) -> Result<Value, DispatchError> {
    let mut input = fixture.input.clone();
    input["admission"] = admission;
    fixture
        .runtime
        .run_deterministic("review_gate_settle", &json!({}), &input, Default::default())
}

/// An admitted gate records the fields job templates and settlement read, in
/// their recorded order, and one recorded before `held_attempt_id`,
/// `owed_evidence`, `owner_owed_evidence`, `host_validation` and
/// `evidence_carry` existed settles the same candidate.
#[test]
fn admitted_output_keeps_its_recorded_shape_and_an_older_one_settles() {
    let mut fixture = Fixture::new();
    fixture.admit();
    let admission = fixture.input["admission"].clone();
    let started_at = admission["started_at"].as_str().unwrap();
    assert!(
        started_at.ends_with("+00:00"),
        "started_at keeps its RFC 3339 offset spelling: {started_at}"
    );
    let recorded = json!({
        "applies": true,
        "decision": "admitted",
        "held_attempt_id": null,
        "first_task_id": fixture.task_id,
        "attempt_id": admission["attempt_id"],
        "attempt_index": 1,
        "lineage_key": admission["lineage_key"],
        "timing": "before-pr",
        "timing_source": admission["timing_source"],
        "reviewer": {
            "crew": "reviewers",
            "crew_source": admission["reviewer"]["crew_source"],
            "provider": "codex",
            "model": "review-model",
            "reasoning_effort": null,
        },
        "owed_evidence": [],
        "owner_owed_evidence": [],
        "host_validation": [],
        "base_sha": admission["base_sha"],
        "base_tree": admission["base_tree"],
        "head_sha": admission["head_sha"],
        "head_tree": admission["head_tree"],
        "implementation_commit_count": 1,
        "task_meaning_digest": admission["task_meaning_digest"],
        "task_selectors": {fixture.task_id.clone(): ["file:candidate.txt"]},
        "evidence_carry": null,
        "manifest_artifact": "review-manifest.json",
        "report_artifact": "review-report.json",
        "budget": {"minutes": 10},
        "remaining": admission["remaining"],
        "started_at": started_at,
    });
    assert_eq!(bytes(&admission), bytes(&recorded));

    // Every decision a template compares against keeps its spelling.
    for decision in ["resumed", "evidence_received"] {
        let mut other = recorded.clone();
        other["decision"] = json!(decision);
        let parsed = ReviewGateAdmissionOutput::from_value(&other).unwrap();
        assert_eq!(parsed.decision(), Some(decision));
        assert_eq!(
            bytes(&serde_json::to_value(&parsed).unwrap()),
            bytes(&other)
        );
    }

    let mut older = recorded;
    for late in [
        "held_attempt_id",
        "owed_evidence",
        "owner_owed_evidence",
        "host_validation",
        "evidence_carry",
    ] {
        older.as_object_mut().unwrap().shift_remove(late);
    }
    // The fields `task_pr_pipeline.yaml` and `task_claimed_pr_pipeline.yaml`
    // read from the older output parse to the recorded values, and a
    // re-recorded output keeps each of them unchanged.
    let parsed = ReviewGateAdmissionOutput::from_value(&older).unwrap();
    assert!(parsed.applies());
    assert_eq!(parsed.decision(), Some("admitted"));
    let typed = parsed.admitted().unwrap();
    assert_eq!(typed.first_task_id, fixture.task_id);
    assert_eq!(typed.attempt_id, admission["attempt_id"].as_str().unwrap());
    assert_eq!(
        typed.lineage_key,
        admission["lineage_key"].as_str().unwrap()
    );
    assert_eq!(typed.reviewer.crew, "reviewers");
    assert_eq!(typed.manifest_artifact, "review-manifest.json");
    assert_eq!(typed.report_artifact, "review-report.json");
    let rerecorded = serde_json::to_value(&parsed).unwrap();
    for (key, value) in older.as_object().unwrap() {
        assert_eq!(&rerecorded[key], value, "re-recorded `{key}`");
    }

    // Templates pass the whole older output to settlement as `admission`.
    fixture.report("accept");
    let settled = settle_with(&fixture, older).unwrap();
    assert_eq!(settled["gate"], "passed", "{settled}");
    assert_eq!(settled["reviewed_head_sha"], admission["head_sha"]);
    assert_eq!(settled["reviewed_base_sha"], admission["base_sha"]);
}

/// A gate that does not apply records `applies: false` with its reason and
/// the captured timing, `null`s when no admission was captured, and
/// settlement passes the reason through. The budget preflight records only
/// its decision and lineage.
#[test]
fn not_applicable_and_preflight_outputs_keep_their_recorded_shape() {
    let mut fixture = Fixture::before_landing();
    fixture.admit();
    let timing_source = fixture
        .runtime
        .operation_policy()
        .review_before_landing
        .source
        .label()
        .to_string();
    let not_applicable = json!({
        "applies": false,
        "reason": "review_before_landing",
        "timing": "before-landing",
        "timing_source": timing_source,
    });
    assert_eq!(bytes(&fixture.input["admission"]), bytes(&not_applicable));

    let missing = json!({
        "applies": false,
        "reason": "review_admission_missing",
        "timing": null,
        "timing_source": null,
    });
    for (admission, reason) in [
        (not_applicable, "review_before_landing"),
        (missing, "review_admission_missing"),
    ] {
        let settled = settle_with(&fixture, admission).unwrap();
        assert_eq!(settled["gate"], "not_required", "{settled}");
        assert_eq!(settled["reason"], reason, "{settled}");
    }

    let mut input = fixture.input.clone();
    input["preflight"] = json!(true);
    let preflight = fixture
        .runtime
        .run_deterministic("review_gate_admit", &json!({}), &input, Default::default())
        .unwrap();
    let lineage_key = preflight["lineage_key"].as_str().unwrap();
    assert!(!lineage_key.is_empty(), "{preflight}");
    assert_eq!(
        bytes(&preflight),
        bytes(&json!({
            "applies": true,
            "decision": "preflight_passed",
            "lineage_key": lineage_key,
        }))
    );
}
