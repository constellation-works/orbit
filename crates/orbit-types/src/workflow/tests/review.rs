//! [ORB-14192] Retained review report revisions and the evidence fields a
//! reviewer attaches.

use chrono::{TimeZone, Utc};

use crate::workflow::{
    NegativeControl, REVIEW_REPORT_HISTORY_LIMIT, ReviewReport, ReviewReportHistory,
    ReviewReportRevision, ReviewVerdict, ValidationRole,
};

fn revision(attempt: &str, sha: &str) -> ReviewReportRevision {
    ReviewReportRevision {
        attempt_id: attempt.to_string(),
        sha256: sha.to_string(),
        observed_at: Utc.with_ymd_and_hms(2026, 10, 5, 9, 54, 0).unwrap(),
        recorded_by: "reviewer".to_string(),
        verdict: ReviewVerdict::Incomplete,
        validation: Vec::new(),
    }
}

/// A full history makes room only by dropping another attempt's oldest
/// revision; an attempt that fills it alone is refused rather than losing its
/// own obligations, and a retried put of the same bytes records nothing.
#[test]
fn the_bounded_history_never_evicts_the_recording_attempts_own_revisions() {
    let mut history = ReviewReportHistory::default();
    assert_eq!(history.record(revision("old", "a")), Ok(true));
    for index in 1..REVIEW_REPORT_HISTORY_LIMIT {
        assert_eq!(
            history.record(revision("live", &index.to_string())),
            Ok(true)
        );
    }
    assert_eq!(
        history.record(revision("live", "1")),
        Ok(false),
        "the same attempt's identical bytes are already retained"
    );

    assert_eq!(history.record(revision("live", "last")), Ok(true));
    assert_eq!(history.revisions.len(), REVIEW_REPORT_HISTORY_LIMIT);
    assert_eq!(
        history.for_attempt("old").count(),
        0,
        "another attempt made room"
    );
    assert_eq!(
        history.for_attempt("live").count(),
        REVIEW_REPORT_HISTORY_LIMIT
    );

    let refused = history
        .record(revision("live", "overflow"))
        .expect_err("one attempt filling the history is refused");
    assert!(refused.contains("admit a fresh review"), "{refused}");
    assert_eq!(
        history.for_attempt("live").count(),
        REVIEW_REPORT_HISTORY_LIMIT
    );
}

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
