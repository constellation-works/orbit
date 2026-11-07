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
