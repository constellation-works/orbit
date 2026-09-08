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
        check: None,
    }
}

fn with_check(mut record: ReviewValidation, check: &str) -> ReviewValidation {
    record.check = Some(check.into());
    record
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
    // A same-command rerun is an unambiguous replacement without an extra
    // identity field.
    let same_command = vec![
        record(
            "cargo test --package orbit-core",
            ValidationOutcome::Failed,
            ValidationRole::Superseded,
            Some("first attempt; rerun below after the repair"),
        ),
        required("cargo test --package orbit-core", ValidationOutcome::Passed),
    ];
    assert_eq!(validation_evidence(&same_command), Ok(()));

    // The shape ORB-11516 recorded: a leaked sandbox allowlist failed the
    // first attempt, and the corrected environment passed. The commands
    // differ, so both records name the same check identity. The failure
    // stays in the record.
    let corrected = vec![
        with_check(
            record(
                "cargo test --package orbit-core",
                ValidationOutcome::Failed,
                ValidationRole::Superseded,
                Some("sandbox allowlist leak; rerun below in a corrected environment"),
            ),
            "orbit-core-tests",
        ),
        with_check(
            required(
                "ORBIT_TEST_ALLOWLIST=1 cargo test --package orbit-core",
                ValidationOutcome::Passed,
            ),
            "orbit-core-tests",
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
fn an_unrelated_later_required_pass_does_not_replace_a_superseded_test() {
    // The ORB-11528 gap: a failed test classified superseded, followed only
    // by an unrelated formatter, was treated as replaced.
    let unrelated = vec![
        record(
            "cargo test",
            ValidationOutcome::Failed,
            ValidationRole::Superseded,
            Some("rerun after repair"),
        ),
        required("cargo fmt --check", ValidationOutcome::Passed),
    ];
    assert_eq!(
        validation_evidence(&unrelated),
        Err(ValidationDefect::SupersededWithoutReplacement {
            command: "cargo test".into(),
        }),
        "a later formatter is not the check that replaced the test"
    );
}

#[test]
fn one_sided_check_ids_do_not_collide_with_fallback_commands() {
    let superseded_check = vec![
        with_check(
            record(
                "cargo test",
                ValidationOutcome::Failed,
                ValidationRole::Superseded,
                Some("rerun after repair"),
            ),
            "cargo fmt --check",
        ),
        required("cargo fmt --check", ValidationOutcome::Passed),
    ];
    assert_eq!(
        validation_evidence(&superseded_check),
        Err(ValidationDefect::SupersededWithoutReplacement {
            command: "cargo test".into(),
        }),
        "an explicit superseded check ID cannot match a required command"
    );

    let required_check = vec![
        record(
            "cargo fmt --check",
            ValidationOutcome::Failed,
            ValidationRole::Superseded,
            Some("rerun after repair"),
        ),
        with_check(
            required("cargo test", ValidationOutcome::Passed),
            "cargo fmt --check",
        ),
    ];
    assert_eq!(
        validation_evidence(&required_check),
        Err(ValidationDefect::SupersededWithoutReplacement {
            command: "cargo fmt --check".into(),
        }),
        "a required check ID cannot match a superseded fallback command"
    );
}

#[test]
fn missing_ambiguous_invalid_or_non_passing_replacement_relationships_fail_closed() {
    let superseded = |check: Option<&str>| {
        let record = record(
            "cargo test --package orbit-core",
            ValidationOutcome::Failed,
            ValidationRole::Superseded,
            Some("sandbox allowlist leak"),
        );
        match check {
            Some(identity) => with_check(record, identity),
            None => record,
        }
    };

    // Corrected command with no shared identity: missing relationship.
    let missing_identity = vec![
        superseded(None),
        required(
            "ORBIT_TEST_ALLOWLIST=1 cargo test --package orbit-core",
            ValidationOutcome::Passed,
        ),
    ];
    assert_eq!(
        validation_evidence(&missing_identity),
        Err(ValidationDefect::SupersededWithoutReplacement {
            command: "cargo test --package orbit-core".into(),
        }),
        "a different command is not a replacement without a shared check identity"
    );

    // Identity on only one side does not bind to the other record's command.
    let one_sided = vec![
        superseded(Some("orbit-core-tests")),
        required(
            "ORBIT_TEST_ALLOWLIST=1 cargo test --package orbit-core",
            ValidationOutcome::Passed,
        ),
    ];
    assert_eq!(
        validation_evidence(&one_sided),
        Err(ValidationDefect::SupersededWithoutReplacement {
            command: "cargo test --package orbit-core".into(),
        }),
        "a one-sided check identity is not an unambiguous replacement"
    );

    // Empty and whitespace identities match nothing.
    for invalid in ["", "   "] {
        let invalid_identity = vec![
            with_check(
                record(
                    "cargo test",
                    ValidationOutcome::Failed,
                    ValidationRole::Superseded,
                    Some("rerun after repair"),
                ),
                invalid,
            ),
            required("cargo test", ValidationOutcome::Passed),
        ];
        assert_eq!(
            validation_evidence(&invalid_identity),
            Err(ValidationDefect::SupersededWithoutReplacement {
                command: "cargo test".into(),
            }),
            "an empty or whitespace check identity is invalid: {invalid:?}"
        );
    }

    // A related later required check that did not pass is not a replacement.
    let related_failed = vec![
        with_check(
            record(
                "cargo test --package orbit-core",
                ValidationOutcome::Failed,
                ValidationRole::Superseded,
                Some("sandbox allowlist leak"),
            ),
            "orbit-core-tests",
        ),
        with_check(
            required(
                "ORBIT_TEST_ALLOWLIST=1 cargo test --package orbit-core",
                ValidationOutcome::Failed,
            ),
            "orbit-core-tests",
        ),
    ];
    assert_eq!(
        validation_evidence(&related_failed),
        Err(ValidationDefect::SupersededWithoutReplacement {
            command: "cargo test --package orbit-core".into(),
        }),
        "a related required check that failed is not a replacement"
    );

    // Identity present but pointing at a different check is not a replacement.
    let mismatched = vec![
        with_check(
            record(
                "cargo test",
                ValidationOutcome::Failed,
                ValidationRole::Superseded,
                Some("rerun after repair"),
            ),
            "unit-tests",
        ),
        with_check(
            required("cargo fmt --check", ValidationOutcome::Passed),
            "formatting",
        ),
    ];
    assert_eq!(
        validation_evidence(&mismatched),
        Err(ValidationDefect::SupersededWithoutReplacement {
            command: "cargo test".into(),
        }),
        "distinct check identities are not a replacement relationship"
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
    assert!(
        legacy.iter().all(|r| r.check.is_none()),
        "role-less evidence does not gain a check identity"
    );
    assert_eq!(
        validation_evidence(&legacy),
        Err(ValidationDefect::RequiredNotPassed {
            command: "deploy".into(),
            outcome: ValidationOutcome::NotRun,
        })
    );
}
