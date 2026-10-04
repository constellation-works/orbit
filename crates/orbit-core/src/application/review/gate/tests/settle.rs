//! Settling released attempts, drifted reports and bundle reports.

use std::fs;

use chrono::{Duration, Utc};
use orbit_engine::DispatchError;
use orbit_types::workflow::{FindingDisposition, ReviewAttemptState, ReviewVerdict};
use serde_json::json;

use super::support::{
    BEFORE_PR, gated_bundle_fixture, gated_fixture, report, write_report, write_report_bytes,
};

#[test]
fn a_failed_reviewer_step_leaves_no_open_attempt_and_a_resume_settles_it() {
    let gated = gated_fixture(BEFORE_PR);
    gated.start(&gated.run_id, Utc::now() - Duration::minutes(10));
    let admission = gated.admit().expect("admit");
    let attempt_id = admission["attempt_id"]
        .as_str()
        .expect("attempt")
        .to_string();

    gated.release(&gated.run_id, &admission);
    let ledger = gated.ledger(&admission);
    assert!(
        ledger.open_attempt().is_none(),
        "a failed reviewer step never leaves its attempt open"
    );
    let released = &ledger.attempts[0];
    assert_eq!(
        released.state,
        ReviewAttemptState::Settled {
            verdict: ReviewVerdict::Incomplete
        }
    );
    assert!(released.released_at.is_some());
    assert!(
        ledger.consumed_seconds < 60,
        "only reviewer runtime is charged, not the run's earlier ten minutes: {}s",
        ledger.consumed_seconds
    );

    let resumed = gated.resume(&gated.run_id);
    write_report(
        &gated.fixture.runtime,
        &gated.task_id,
        &report(&attempt_id, ReviewVerdict::PassedWithoutRepairs, false),
    );
    let settled = gated
        .settle_in(&resumed, &admission)
        .expect("the resumed run settles the released attempt");
    assert_eq!(settled["gate"], "passed");
    let ledger = gated.ledger(&admission);
    assert_eq!(ledger.attempts.len(), 1, "settling consumed no new start");
    assert_eq!(
        ledger.attempts[0].state,
        ReviewAttemptState::Settled {
            verdict: ReviewVerdict::PassedWithoutRepairs
        }
    );
    assert!(ledger.attempts[0].released_at.is_none());

    gated.release(&resumed, &admission);
    assert_eq!(
        gated.ledger(&admission).attempts[0].state,
        ReviewAttemptState::Settled {
            verdict: ReviewVerdict::PassedWithoutRepairs
        },
        "a release never rewrites a settled verdict"
    );
}

#[test]
fn a_drifted_report_on_a_later_bundle_task_still_settles() {
    let gated = gated_bundle_fixture(BEFORE_PR, 2);
    let admission = gated.admit().expect("admit");
    fs::write(
        gated.fixture.repo.join("src.txt"),
        "implementation target\nimplemented\nreviewed\n",
    )
    .expect("reviewer repair");
    let drifted = json!({
        "schema_version": "1",
        "attempt_id": admission["attempt_id"],
        "verdict": "Passed-With-Repairs",
        "summary": "Added the missing note.",
        "findings": [{
            "id": 1,
            "severity": "low",
            "summary": "Missing note",
            "paths": "src.txt",
            "disposition": "repaired",
        }],
        "validation": [{"command": "make ci-fast", "outcome": "PASS"}],
    });
    write_report_bytes(
        &gated.fixture.runtime,
        &gated.bundle[1],
        serde_json::to_vec(&drifted).expect("serialize"),
    );

    let settled = gated
        .settle(&admission)
        .expect("drift that keeps the report's meaning settles");
    assert_eq!(settled["verdict"], "passed_with_repairs");
    let certificate = gated.certificate();
    assert_eq!(certificate.findings.len(), 1);
    assert_eq!(certificate.findings[0].id, "1");
    assert_eq!(
        certificate.findings[0].disposition,
        FindingDisposition::Repaired
    );
}

#[test]
fn the_most_severe_bundle_report_decides_and_refuses_without_retry() {
    let gated = gated_bundle_fixture(BEFORE_PR, 2);
    let admission = gated.admit().expect("admit");
    let attempt_id = admission["attempt_id"].as_str().expect("attempt");
    write_report(
        &gated.fixture.runtime,
        &gated.bundle[0],
        &report(attempt_id, ReviewVerdict::PassedWithoutRepairs, false),
    );
    write_report(
        &gated.fixture.runtime,
        &gated.bundle[1],
        &report(attempt_id, ReviewVerdict::ChangesRequired, false),
    );

    let error = gated
        .settle(&admission)
        .expect_err("changes required blocks");
    assert!(
        matches!(error, DispatchError::DeterministicActionRefused { .. }),
        "a settled verdict is a decision, not a retryable fault: {error}"
    );
    assert!(error.to_string().contains("review_gate_blocked"), "{error}");
    assert_eq!(gated.certificate().verdict, ReviewVerdict::ChangesRequired);
}
