//! [ORB-14192] The evidence fields a reviewer attaches to its report.

use orbit_types::workflow::{NegativeControl, ReviewReport, ReviewReportError, ValidationRole};

/// A drifted `control` label and a single `sources` or `mutation_target`
/// string read as the contract; an unknown control is refused naming its
/// field.
#[test]
fn control_and_sources_tolerate_drift_and_name_an_unknown_kind() {
    let report = ReviewReport::parse(
        br#"{"schema_version":1,"attempt_id":"rvw-1","verdict":"accept","summary":"",
            "validation":[{"command":"cargo test fix","outcome":"fail",
              "role":"Expected-Failure","control":"Pre-Fix","sources":"src/fix.rs",
              "note":"pre-fix reproduction"},
              {"command":"cargo test guard","outcome":"failed","role":"expected_failure",
              "control":"counterfactual","sources":["tests/guard.rs"],
              "mutation_target":"src/guarded.rs","note":"mutated the guarded check"}]}"#,
    )
    .expect("drift that leaves the meaning unambiguous reads");
    let record = &report.validation[0];
    assert_eq!(record.role, ValidationRole::ExpectedFailure);
    assert_eq!(record.control, Some(NegativeControl::PreFix));
    assert_eq!(record.sources, vec!["src/fix.rs".to_string()]);
    assert!(record.mutation_target.is_empty());
    let counterfactual = &report.validation[1];
    assert_eq!(counterfactual.sources, vec!["tests/guard.rs".to_string()]);
    assert_eq!(
        counterfactual.mutation_target,
        vec!["src/guarded.rs".to_string()]
    );

    let error = ReviewReport::parse(
        br#"{"schema_version":1,"attempt_id":"rvw-1","verdict":"accept","summary":"",
            "validation":[{"command":"cargo test","outcome":"failed",
              "role":"expected_failure","control":"flaky","note":"n"}]}"#,
    )
    .expect_err("an unknown control kind is not guessed");
    assert!(
        matches!(
            &error,
            ReviewReportError::Contract { detail } if detail.starts_with("validation[0].control:")
        ),
        "{error}"
    );
}

/// A complete Git diff may name a legal Git path that the selector cannot
/// normalize. That uncertainty must never certify a non-match.
#[test]
fn unrepresentable_changed_path_holds_selection_and_owner_snapshot_is_bound() {
    use orbit_types::workflow::{
        SelectiveBeforeLandingMode, SelectiveBeforeLandingPolicy, SelectiveEvaluationInputs,
        SelectiveReviewResult, SelectiveUnknownRisk, evaluate_selective_review,
    };
    let mut policy = SelectiveBeforeLandingPolicy::new(
        SelectiveBeforeLandingMode::Enforce,
        Some("0.5".into()),
        vec!["**".into()],
        SelectiveUnknownRisk::Skip,
        Default::default(),
    );
    policy.freeze_risk_model("owner-risk-model/unknown".into());
    policy.validate().expect("owner snapshot sealed");
    let receipt = evaluate_selective_review(
        &policy,
        "workspace",
        "run",
        &["ORB-20087".into()],
        ("base", "base-tree"),
        ("head", "head-tree"),
        SelectiveEvaluationInputs {
            changed_paths: vec![(None, Some("~".into()))],
            ..Default::default()
        },
    );
    assert_eq!(receipt.result, SelectiveReviewResult::Unavailable);
    assert!(!receipt.gaps.is_empty());
    policy.risk_model_identity = Some("leaf intercept=-100 slope=0".into());
    assert!(
        policy.validate().is_err(),
        "a receipt cannot substitute a leaf fit"
    );
}

/// A fixed vision finding is not resolved by an unavailable recapture.
#[test]
fn fixed_visual_finding_requires_established_recapture() {
    use orbit_types::task::VisualReviewRecord;
    use serde_json::json;
    let mut value = json!({
        "schema_version":1,"purpose":"visual_review","mode":"live","task_id":"T-1",
        "candidate_sha":"original","outcome":"reviewed","recorded_only":false,
        "budget":{"max_mocks":1,"max_images":2,"max_image_tokens":1000,"max_findings":1,
            "images_shown":2,"estimated_image_tokens":1000},
        "invalid_evidence_dropped":0,"over_max_findings_dropped":0,
        "findings":[{"finding_id":"f1","source":"comparison","question":"1","mock":"mocks/view.png",
            "title":"missing action","detail":"action absent","severity":"high",
            "verdict":{"verdict":"confirmed","reasoning":"verified"},
            "disposition":"must_address","resolution":"fixed"}]
    });
    for outcome in [None, Some("not_established"), Some("incomplete")] {
        if let Some(outcome) = outcome {
            value["recapture"] =
                json!({"candidate_sha":"fixed","outcome":outcome,"still_failing":[]});
        }
        let record = VisualReviewRecord::parse(&serde_json::to_vec(&value).unwrap()).unwrap();
        assert_eq!(
            record.unresolved_blocking().len(),
            1,
            "unavailable recapture must hold: {outcome:?}"
        );
    }
    value["recapture"] = json!({"candidate_sha":"fixed","outcome":"passed","still_failing":[]});
    let record = VisualReviewRecord::parse(&serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(record.unresolved_blocking().is_empty());
}
