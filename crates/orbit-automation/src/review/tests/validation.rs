//! What a reviewer's validation records establish [ORB-11528].

use orbit_types::workflow::{ReviewValidation, ValidationOutcome, ValidationRole};

use crate::review::{ValidationDefect, validation_evidence, validation_role_counts};

fn record(
    command: &str,
    outcome: ValidationOutcome,
    role: ValidationRole,
    note: Option<&str>,
) -> ReviewValidation {
    ReviewValidation {
        command: command.into(),
        outcome,
        role,
        note: note.map(Into::into),
    }
}

fn required(command: &str, outcome: ValidationOutcome) -> ReviewValidation {
    record(command, outcome, ValidationRole::Required, None)
}

#[test]
fn a_negative_control_and_a_scope_exclusion_coexist_with_passing_required_checks() {
    // The shape ORB-11511 recorded: the superseded assertion had to fail,
    // the deployment was never authorized, and the checks that bind the
    // candidate passed.
    let records = vec![
        required("make ci-fast", ValidationOutcome::Passed),
        required("make ci-lint", ValidationOutcome::Passed),
        record(
            "grep -q 'Strict-Transport-Security' old-config",
            ValidationOutcome::Failed,
            ValidationRole::ExpectedFailure,
            Some("negative control: the superseded assertion must no longer hold"),
        ),
        record(
            "wrangler deploy",
            ValidationOutcome::NotRun,
            ValidationRole::Excluded,
            Some("live deployment is explicitly outside the authorized scope"),
        ),
    ];
    assert_eq!(validation_evidence(&records), Ok(()));
    assert_eq!(
        validation_role_counts(&records),
        vec![
            (ValidationRole::Required, 2),
            (ValidationRole::ExpectedFailure, 1),
            (ValidationRole::Excluded, 1),
        ]
    );
}

#[test]
fn required_checks_that_did_not_pass_still_block() {
    for outcome in [
        ValidationOutcome::Failed,
        ValidationOutcome::Denied,
        ValidationOutcome::NotRun,
    ] {
        let records = vec![
            required("cargo test", ValidationOutcome::Passed),
            required("make ci-lint", outcome),
        ];
        assert_eq!(
            validation_evidence(&records),
            Err(ValidationDefect::RequiredNotPassed {
                command: "make ci-lint".into(),
                outcome,
            }),
            "a {} required check must block a pass",
            outcome.as_str()
        );
    }

    // A denied required check keeps its own label: the runner refused, which
    // says nothing about the candidate.
    let denied = vec![required("make ci-lint", ValidationOutcome::Denied)];
    assert!(
        validation_evidence(&denied)
            .unwrap_err()
            .reason()
            .starts_with("validation_unavailable:"),
        "a denied runner is unavailable validation, not a defect"
    );
}

#[test]
fn a_set_without_a_passing_required_check_establishes_nothing() {
    assert_eq!(
        validation_evidence(&[]),
        Err(ValidationDefect::NoRequiredCheck)
    );

    // Controls and exclusions alone are not coverage, however honest.
    let no_candidate_check = vec![
        record(
            "cargo test old_assertion",
            ValidationOutcome::Failed,
            ValidationRole::ExpectedFailure,
            Some("the pre-fix reproduction must fail"),
        ),
        record(
            "wrangler deploy",
            ValidationOutcome::NotRun,
            ValidationRole::Excluded,
            Some("out of scope"),
        ),
    ];
    assert_eq!(
        validation_evidence(&no_candidate_check),
        Err(ValidationDefect::NoRequiredCheck)
    );
}

#[test]
fn an_outcome_that_contradicts_its_classification_blocks() {
    let control_passed = vec![
        required("make ci-fast", ValidationOutcome::Passed),
        record(
            "cargo test old_assertion",
            ValidationOutcome::Passed,
            ValidationRole::ExpectedFailure,
            Some("the pre-fix reproduction must fail"),
        ),
    ];
    assert_eq!(
        validation_evidence(&control_passed),
        Err(ValidationDefect::RoleContradicted {
            command: "cargo test old_assertion".into(),
            role: ValidationRole::ExpectedFailure,
            outcome: ValidationOutcome::Passed,
        }),
        "a negative control that passed disproves what it was recorded for"
    );

    let exclusion_ran = vec![
        required("make ci-fast", ValidationOutcome::Passed),
        record(
            "wrangler deploy",
            ValidationOutcome::Passed,
            ValidationRole::Excluded,
            Some("out of scope"),
        ),
    ];
    assert_eq!(
        validation_evidence(&exclusion_ran),
        Err(ValidationDefect::RoleContradicted {
            command: "wrangler deploy".into(),
            role: ValidationRole::Excluded,
            outcome: ValidationOutcome::Passed,
        }),
        "an excluded action that ran was not excluded"
    );

    // A runner that refused the excluded action agrees with the exclusion.
    let exclusion_denied = vec![
        required("make ci-fast", ValidationOutcome::Passed),
        record(
            "wrangler deploy",
            ValidationOutcome::Denied,
            ValidationRole::Excluded,
            Some("out of scope; the runner refuses it too"),
        ),
    ];
    assert_eq!(validation_evidence(&exclusion_denied), Ok(()));
}

#[test]
fn a_superseded_attempt_needs_the_later_check_that_replaced_it() {
    // The shape ORB-11516 recorded: a leaked sandbox allowlist failed the
    // first attempt, and the corrected environment passed. The failure stays
    // in the record.
    let corrected = vec![
        record(
            "cargo test --package orbit-core",
            ValidationOutcome::Failed,
            ValidationRole::Superseded,
            Some("sandbox allowlist leak; rerun below in a corrected environment"),
        ),
        required(
            "ORBIT_TEST_ALLOWLIST=1 cargo test --package orbit-core",
            ValidationOutcome::Passed,
        ),
    ];
    assert_eq!(validation_evidence(&corrected), Ok(()));
    assert_eq!(
        validation_role_counts(&corrected),
        vec![
            (ValidationRole::Required, 1),
            (ValidationRole::Superseded, 1),
        ],
        "the failed observation is preserved, not erased"
    );

    // Nothing after it: the diagnostic never reached a final-candidate check.
    let unresolved = vec![
        required("make ci-fast", ValidationOutcome::Passed),
        record(
            "cargo test --package orbit-core",
            ValidationOutcome::Failed,
            ValidationRole::Superseded,
            Some("sandbox allowlist leak"),
        ),
    ];
    assert_eq!(
        validation_evidence(&unresolved),
        Err(ValidationDefect::SupersededWithoutReplacement {
            command: "cargo test --package orbit-core".into(),
        }),
        "a check recorded before the attempt cannot resolve it"
    );
}

#[test]
fn an_unexplained_reclassification_is_refused_and_legacy_records_stay_required() {
    for role in [
        ValidationRole::ExpectedFailure,
        ValidationRole::Excluded,
        ValidationRole::Superseded,
    ] {
        let records = vec![
            required("make ci-fast", ValidationOutcome::Passed),
            record("cargo test", ValidationOutcome::Failed, role, Some("  ")),
        ];
        assert_eq!(
            validation_evidence(&records),
            Err(ValidationDefect::ClassificationUnexplained {
                command: "cargo test".into(),
                role,
            }),
            "{} needs a note that says why",
            role.as_str()
        );
    }

    // Evidence written before the contract carries no role and keeps its
    // conservative meaning: every recorded command is a required check.
    let legacy: Vec<ReviewValidation> = serde_json::from_str(
        r#"[{"command":"make ci-fast","outcome":"passed"},
            {"command":"deploy","outcome":"not_run"}]"#,
    )
    .expect("legacy validation records");
    assert!(legacy.iter().all(|r| r.role == ValidationRole::Required));
    assert_eq!(
        validation_evidence(&legacy),
        Err(ValidationDefect::RequiredNotPassed {
            command: "deploy".into(),
            outcome: ValidationOutcome::NotRun,
        })
    );
}
