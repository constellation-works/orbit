//! [ORB-14192] The evidence fields a reviewer attaches to its report.

use crate::workflow::{NegativeControl, ReviewReport, ValidationRole};

/// A drifted `control` label and a single `sources` string read as the
/// contract; an unknown control is refused naming its field.
#[test]
fn control_and_sources_tolerate_drift_and_name_an_unknown_kind() {
    let report = ReviewReport::parse(
        br#"{"schema_version":1,"attempt_id":"rvw-1","verdict":"accept","summary":"",
            "validation":[{"command":"cargo test fix","outcome":"fail",
              "role":"Expected-Failure","control":"Pre-Fix","sources":"src/fix.rs",
              "note":"pre-fix reproduction"}]}"#,
    )
    .expect("drift that leaves the meaning unambiguous reads");
    let record = &report.validation[0];
    assert_eq!(record.role, ValidationRole::ExpectedFailure);
    assert_eq!(record.control, Some(NegativeControl::PreFix));
    assert_eq!(record.sources, vec!["src/fix.rs".to_string()]);

    let error = ReviewReport::parse(
        br#"{"schema_version":1,"attempt_id":"rvw-1","verdict":"accept","summary":"",
            "validation":[{"command":"cargo test","outcome":"failed",
              "role":"expected_failure","control":"flaky","note":"n"}]}"#,
    )
    .expect_err("an unknown control kind is not guessed");
    assert!(error.starts_with("validation[0].control:"), "{error}");
}
