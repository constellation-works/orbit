//! [ORB-14616] A report whose only defect is its shape goes back to the
//! reviewer before settlement, and the corrected report settles in the same
//! attempt.

use orbit_engine::ReviewReportCorrectionRequest;
use orbit_types::workflow::{
    ReviewAttemptState, ReviewValidation, ReviewVerdict, ValidationOutcome, ValidationRole,
};
use serde_json::Value;

use super::super::review_report_correction;
use super::support::{BEFORE_PR, Gated, counterfactual, gated_fixture, report, write_report};

const GOLDENS: &str = "make goldens";
const LINT: &str = "make ci-lint";

/// `BEFORE_PR` with `make goldens` a baseline command and `make ci-lint` a
/// required one, as the owner captures them at admission.
fn owner_checks() -> String {
    format!(
        "{}baseline_commands = [\"{GOLDENS}\"]\n",
        BEFORE_PR.replace(
            "[workflow]\n",
            &format!("[workflow]\nrequired_validation_commands = [\"{LINT}\"]\n")
        )
    )
}

/// A check the reviewer left unrun, filed under `role`.
fn unrun(command: &str, role: ValidationRole) -> ReviewValidation {
    let mut record = report("attempt", ReviewVerdict::Accept, false)
        .validation
        .remove(0);
    record.id = None;
    record.command = command.to_string();
    record.outcome = ValidationOutcome::NotRun;
    record.role = role;
    record.note = Some("left unrun in this lane".to_string());
    record
}

/// A passing `make ci-lint`, which `owner_checks` requires of every candidate.
fn lint_passed() -> ReviewValidation {
    let mut record = unrun(LINT, ValidationRole::Required);
    record.id = Some("V2".to_string());
    record.outcome = ValidationOutcome::Passed;
    record.note = None;
    record
}

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

/// [ORB-14632] A mutation target spelled as an absolute path into the review
/// worktree cannot be compared against the repository. It is a shape defect
/// the reviewer corrects to the repository-relative path before settlement.
#[test]
fn an_absolute_mutation_target_is_returned_for_correction() {
    let gated = gated_fixture(BEFORE_PR);
    let admission = gated.admit().expect("admit");
    let attempt_id = admission["attempt_id"].as_str().expect("attempt");
    let absolute = gated.fixture.repo.join("README.md").display().to_string();
    let mut absolute_target = report(attempt_id, ReviewVerdict::Accept, false);
    absolute_target
        .validation
        .push(counterfactual(&["src.txt"], &[absolute.as_str()]));
    write_report(&gated.fixture.runtime, &gated.task_id, &absolute_target);

    let defect = correction(&gated, &admission).expect("a shape defect is returned");
    assert!(
        defect.contains(&format!("mutation_target `{absolute}`"))
            && defect.contains("not a repository-relative path"),
        "{defect}"
    );

    let mut corrected = report(attempt_id, ReviewVerdict::Accept, false);
    corrected
        .validation
        .push(counterfactual(&["src.txt"], &["README.md"]));
    write_report(&gated.fixture.runtime, &gated.task_id, &corrected);
    assert_eq!(correction(&gated, &admission), None);
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

/// [ORB-15083] A skipped command filed as a `not_run` diagnostic is returned
/// to the reviewer once, with the correction its class takes: a baseline
/// command is recorded `excluded`, and the corrected report
/// settles in the same attempt.
#[test]
fn a_skipped_baseline_command_filed_as_diagnostic_is_returned_and_settles_excluded() {
    let gated = gated_fixture(&owner_checks());
    let admission = gated.admit().expect("admit");
    let attempt_id = admission["attempt_id"].as_str().expect("attempt");
    let mut mislabelled = report(attempt_id, ReviewVerdict::Accept, false);
    mislabelled.validation.push(lint_passed());
    mislabelled
        .validation
        .push(unrun(GOLDENS, ValidationRole::Diagnostic));
    write_report(&gated.fixture.runtime, &gated.task_id, &mislabelled);

    let defect = correction(&gated, &admission).expect("a not_run diagnostic is returned");
    assert!(
        defect.starts_with("validation_contradicted:")
            && defect.contains("`make goldens` was recorded as diagnostic but is not_run")
            && defect.contains("record it `excluded` or omit it"),
        "{defect}"
    );
    assert_eq!(
        gated.ledger(&admission).attempts[0].state,
        ReviewAttemptState::Open,
        "asking for a correction settles nothing"
    );

    let mut corrected = report(attempt_id, ReviewVerdict::Accept, false);
    corrected.validation.push(lint_passed());
    corrected
        .validation
        .push(unrun(GOLDENS, ValidationRole::Excluded));
    write_report(&gated.fixture.runtime, &gated.task_id, &corrected);
    assert_eq!(correction(&gated, &admission), None);
    let settled = gated
        .settle(&admission)
        .expect("the corrected report settles");
    assert_eq!(settled["verdict"], "accept");
    assert_eq!(settled["attempt_id"], attempt_id);
}

/// [ORB-15083] A host-required command cannot be excused by relabelling:
/// the correction says to run it, and a corrected report that records it
/// `excluded` is refused at settlement.
#[test]
fn a_host_required_command_recorded_excluded_after_correction_is_refused() {
    let gated = gated_fixture(&owner_checks());
    let admission = gated.admit().expect("admit");
    let attempt_id = admission["attempt_id"].as_str().expect("attempt");
    let mut mislabelled = report(attempt_id, ReviewVerdict::Accept, false);
    mislabelled
        .validation
        .push(unrun(LINT, ValidationRole::Diagnostic));
    write_report(&gated.fixture.runtime, &gated.task_id, &mislabelled);

    let defect = correction(&gated, &admission).expect("a not_run diagnostic is returned");
    assert!(
        defect.contains("it is a host-required check, so run it and record it `required`"),
        "{defect}"
    );

    let mut excluded = report(attempt_id, ReviewVerdict::Accept, false);
    excluded
        .validation
        .push(unrun(LINT, ValidationRole::Excluded));
    write_report(&gated.fixture.runtime, &gated.task_id, &excluded);
    assert_eq!(
        correction(&gated, &admission),
        None,
        "the refusal is an observation for settlement, not a second correction"
    );
    let error = gated.settle(&admission).expect_err("excluded is refused");
    assert!(error.to_string().contains("review_gate_blocked"), "{error}");
    let escalation = gated.certificate().escalation.unwrap_or_default();
    assert!(
        escalation.contains("host-required check `make ci-lint` is not established"),
        "{escalation}"
    );
}

/// [ORB-15083] The correction is offered once: a report that still carries
/// the `not_run` diagnostic settles `incomplete` with `validation_contradicted`.
#[test]
fn a_not_run_diagnostic_that_survives_the_correction_is_refused_at_settlement() {
    let gated = gated_fixture(&owner_checks());
    let admission = gated.admit().expect("admit");
    let attempt_id = admission["attempt_id"].as_str().expect("attempt");
    let mut mislabelled = report(attempt_id, ReviewVerdict::Accept, false);
    mislabelled.validation.push(lint_passed());
    mislabelled
        .validation
        .push(unrun(GOLDENS, ValidationRole::Diagnostic));
    write_report(&gated.fixture.runtime, &gated.task_id, &mislabelled);
    assert!(correction(&gated, &admission).is_some());

    let error = gated.settle(&admission).expect_err("still contradicted");
    assert!(error.to_string().contains("review_gate_blocked"), "{error}");
    let certificate = gated.certificate();
    assert_eq!(certificate.verdict, ReviewVerdict::Incomplete);
    let escalation = certificate.escalation.unwrap_or_default();
    assert!(
        escalation.contains("validation_contradicted: `make goldens` was recorded as diagnostic"),
        "{escalation}"
    );
}
