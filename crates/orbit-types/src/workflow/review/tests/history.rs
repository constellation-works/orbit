//! [ORB-14192] Retained review report revisions.

use chrono::{TimeZone, Utc};

use crate::workflow::{
    REVIEW_REPORT_HISTORY_LIMIT, ReviewReportHistory, ReviewReportRevision, ReviewVerdict,
};

fn revision(attempt: &str, sha: &str) -> ReviewReportRevision {
    ReviewReportRevision {
        attempt_id: attempt.to_string(),
        sha256: sha.to_string(),
        observed_at: Utc.with_ymd_and_hms(2026, 10, 5, 9, 54, 0).unwrap(),
        recorded_by: "reviewer".to_string(),
        verdict: ReviewVerdict::Incomplete,
        validation: Vec::new(),
        record_id_contract_checked: None,
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
