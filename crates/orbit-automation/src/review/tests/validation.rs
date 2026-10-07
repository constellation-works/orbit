//! What a reviewer's validation records establish [ORB-11528].

use crate::review::{ValidationContext, ValidationDefect, validation_evidence};
use orbit_types::workflow::{ReviewValidation, ValidationOutcome, ValidationRole};

fn record(
    command: &str,
    outcome: ValidationOutcome,
    role: ValidationRole,
    note: Option<&str>,
) -> ReviewValidation {
    ReviewValidation {
        id: None,
        command: command.into(),
        outcome,
        role,
        note: note.map(Into::into),
        check: None,
        control: None,
        sources: Vec::new(),
        baseline: None,
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

    // A changed argument, not a leading environment assignment: a leading
    // `NAME=value` is the same command [ORB-14302].
    let missing_identity = vec![
        superseded(None),
        required(
            "cargo test --package orbit-core --locked",
            ValidationOutcome::Passed,
        ),
    ];
    assert_eq!(
        validation_evidence(&missing_identity, &ValidationContext::default()),
        Err(ValidationDefect::SupersededWithoutReplacement {
            command: "cargo test --package orbit-core".into(),
        }),
        "a different command is not a replacement without a shared check identity"
    );

    // This explicit identity differs from the other record's normalized command.
    let one_sided = vec![
        superseded(Some("orbit-core-tests")),
        required(
            "cargo test --package orbit-core --locked",
            ValidationOutcome::Passed,
        ),
    ];
    assert_eq!(
        validation_evidence(&one_sided, &ValidationContext::default()),
        Err(ValidationDefect::SupersededWithoutReplacement {
            command: "cargo test --package orbit-core".into(),
        }),
        "different effective identities do not identify a replacement"
    );

    // Empty and whitespace identities relate nothing; only a shared command
    // could, and a corrected command is not one.
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
            with_check(
                required("cargo test --workspace", ValidationOutcome::Passed),
                invalid,
            ),
        ];
        assert_eq!(
            validation_evidence(&invalid_identity, &ValidationContext::default()),
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
                "cargo test --package orbit-core --locked",
                ValidationOutcome::Failed,
            ),
            "orbit-core-tests",
        ),
    ];
    assert_eq!(
        validation_evidence(&related_failed, &ValidationContext::default()),
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
        validation_evidence(&mismatched, &ValidationContext::default()),
        Err(ValidationDefect::SupersededWithoutReplacement {
            command: "cargo test".into(),
        }),
        "distinct check identities are not a replacement relationship"
    );
}
