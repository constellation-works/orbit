//! What a reviewer's validation records establish [ORB-11528].

use crate::review::{ValidationDefect, validation_evidence};
use orbit_types::workflow::{ReviewValidation, ValidationOutcome, ValidationRole};

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
