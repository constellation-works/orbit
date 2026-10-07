//! [ORB-14616] A report whose only defect is its shape goes back to the
//! reviewer before settlement, and the corrected report settles in the same
//! attempt.

use orbit_engine::ReviewReportCorrectionRequest;
use orbit_types::workflow::{ReviewAttemptState, ReviewVerdict, ValidationOutcome};
use serde_json::Value;

use super::super::review_report_correction;
use super::support::{BEFORE_PR, Gated, counterfactual, gated_fixture, report, write_report};

fn correction(gated: &Gated, admission: &Value) -> Option<String> {
    review_report_correction(
        &gated.fixture.runtime,
        &ReviewReportCorrectionRequest {
            run_id: gated.run_id.clone(),
            lineage_key: admission["lineage_key"].as_str().expect("lineage").into(),
            attempt_id: admission["attempt_id"].as_str().expect("attempt").into(),
            task_ids: gated.bundle.clone(),
            workspace_path: gated.fixture.repo.clone(),
        },
    )
    .expect("judge the report before settlement")
}

/// The old-shape counterfactual names the out-of-scope file it mutated as a
/// source. Before settlement the host returns that typed defect, nothing is
/// settled, and a report that moves the file to `mutation_target` settles
/// `accept` in the same attempt of the same lineage.
#[test]
fn an_old_shape_counterfactual_is_corrected_and_settles_in_the_same_attempt() {
    let gated = gated_fixture(BEFORE_PR);
    let admission = gated.admit().expect("admit");
    let attempt_id = admission["attempt_id"].as_str().expect("attempt");
    let mut old_shape = report(attempt_id, ReviewVerdict::Accept, false);
    old_shape
        .validation
        .push(counterfactual(&["src.txt", "README.md"], &[]));
    write_report(&gated.fixture.runtime, &gated.task_id, &old_shape);

    let defect = correction(&gated, &admission).expect("a shape defect is returned");
    assert!(
        defect.contains("negative control `make test-guard` names `README.md`")
            && defect.contains("mutation_target"),
        "{defect}"
    );
    let ledger = gated.ledger(&admission);
    assert_eq!(
        ledger.attempts[0].state,
        ReviewAttemptState::Open,
        "asking for a correction settles nothing"
    );
    assert!(ledger.attempts[0].released_at.is_none());

    let mut corrected = report(attempt_id, ReviewVerdict::Accept, false);
    corrected
        .validation
        .push(counterfactual(&["src.txt"], &["README.md"]));
    write_report(&gated.fixture.runtime, &gated.task_id, &corrected);
    assert_eq!(correction(&gated, &admission), None);

    let settled = gated
        .settle(&admission)
        .expect("the corrected report settles");
    assert_eq!(settled["verdict"], "accept");
    assert_eq!(settled["attempt_id"], attempt_id);
    let ledger = gated.ledger(&admission);
    assert_eq!(ledger.attempts.len(), 1, "no second attempt or lineage");
    assert_eq!(
        ledger.attempts[0].state,
        ReviewAttemptState::Settled {
            verdict: ReviewVerdict::Accept
        }
    );
    assert_eq!(gated.certificate().attempt_id, attempt_id);
}

/// A failed required check is what the reviewer observed, not a shape it can
/// correct: the report goes to settlement as it stands.
#[test]
fn a_defect_in_what_the_checks_observed_is_left_to_settlement() {
    let gated = gated_fixture(BEFORE_PR);
    let admission = gated.admit().expect("admit");
    let attempt_id = admission["attempt_id"].as_str().expect("attempt");
    let mut failed = report(attempt_id, ReviewVerdict::Accept, false);
    failed.validation[0].outcome = ValidationOutcome::Failed;
    failed
        .validation
        .push(counterfactual(&["src.txt", "README.md"], &[]));
    write_report(&gated.fixture.runtime, &gated.task_id, &failed);

    assert_eq!(correction(&gated, &admission), None);
}
