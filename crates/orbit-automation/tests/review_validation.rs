#![allow(missing_docs, clippy::expect_used, clippy::unwrap_used)]

//! The review validation gate through the public review-coverage boundary.
//!
//! A reviewer may file a failed attempt as `superseded`. The attempt then
//! counts only when a later required check that passed replaced it: the same
//! command, or the same non-empty `check` identity on both records. Both
//! consumers of these rules are driven here. [`validation_evidence`] gives the
//! reason the issuing gate escalates with. [`certificate_acceptable`] and
//! [`exclusion`] decide whether an issued certificate covers a delivery. A
//! certificate whose `validation_complete` flag is set but whose records do
//! not establish the replacement must never be spent as coverage.

use chrono::{TimeZone, Utc};
use orbit_automation::review::{
    LandingFacts, ValidationDefect, certificate_acceptable, exclusion, validation_evidence,
};
use orbit_types::workflow::automation::{Delivery, SourceRevision};
use orbit_types::workflow::{
    REVIEW_CONTRACT_VERSION, ReviewBudget, ReviewCertificate, ReviewConsumption,
    ReviewInvalidation, ReviewValidation, ReviewVerdict, ReviewerIdentity, ValidationOutcome,
    ValidationRole,
};

const ATTEMPT: &str = "cargo test --package orbit-core";
const CORRECTED: &str = "ORBIT_TEST_ALLOWLIST=1 cargo test --package orbit-core";

#[test]
fn replacement_relationships_that_are_missing_ambiguous_invalid_or_not_passing_fail_closed() {
    struct Case {
        name: &'static str,
        records: Vec<ReviewValidation>,
    }
    let cases = [
        Case {
            name: "missing: a corrected command with no shared check identity",
            records: vec![superseded(ATTEMPT, None), required(CORRECTED, None, true)],
        },
        Case {
            name: "missing: no later required check at all",
            records: vec![
                required("make ci-lint", None, true),
                superseded(ATTEMPT, None),
            ],
        },
        Case {
            name: "missing: the only matching pass precedes the attempt",
            records: vec![required(ATTEMPT, None, true), superseded(ATTEMPT, None)],
        },
        Case {
            name: "ambiguous: an identity on the attempt only",
            records: vec![
                superseded(ATTEMPT, Some("orbit-core-tests")),
                required(CORRECTED, None, true),
            ],
        },
        Case {
            name: "ambiguous: an identity equal to the attempt's command is not that command",
            records: vec![
                superseded(ATTEMPT, None),
                required(CORRECTED, Some(ATTEMPT), true),
            ],
        },
        Case {
            name: "invalid: an empty check identity relates no corrected command",
            records: vec![
                superseded(ATTEMPT, Some("")),
                required(CORRECTED, Some(""), true),
            ],
        },
        Case {
            name: "invalid: a whitespace check identity relates no corrected command",
            records: vec![
                superseded(ATTEMPT, Some("   ")),
                required(CORRECTED, Some("   "), true),
            ],
        },
        Case {
            name: "invalid: identities naming different checks",
            records: vec![
                superseded(ATTEMPT, Some("unit-tests")),
                required(CORRECTED, Some("formatting"), true),
            ],
        },
        Case {
            name: "non-passing: the related replacement is not a pass",
            records: vec![
                superseded(ATTEMPT, Some("orbit-core-tests")),
                record(
                    CORRECTED,
                    Some("orbit-core-tests"),
                    ValidationOutcome::NotRun,
                    ValidationRole::Required,
                    None,
                ),
                required("make ci-fast", None, true),
            ],
        },
        Case {
            name: "non-passing: the related pass is not a required check",
            records: vec![
                superseded(ATTEMPT, Some("orbit-core-tests")),
                record(
                    CORRECTED,
                    Some("orbit-core-tests"),
                    ValidationOutcome::Passed,
                    ValidationRole::Superseded,
                    Some("also superseded"),
                ),
                required("make ci-fast", None, true),
            ],
        },
    ];

    for case in cases {
        let defect = validation_evidence(&case.records)
            .expect_err("an unreplaced superseded attempt must not validate the candidate");
        assert_eq!(
            defect,
            ValidationDefect::SupersededWithoutReplacement {
                command: ATTEMPT.to_string(),
            },
            "{}",
            case.name
        );
        assert!(
            defect.reason().starts_with("validation_incomplete:"),
            "{}: the gate escalates as incomplete validation: {}",
            case.name,
            defect.reason()
        );

        let certificate = certificate(case.records);
        assert_eq!(
            certificate_acceptable(&certificate),
            Err(ReviewInvalidation::ValidationIncomplete),
            "{}: the flag alone is not coverage",
            case.name
        );
        assert_eq!(
            exclusion(&exact_delivery(), &certificate, &facts()).map(|_| ()),
            Err(ReviewInvalidation::ValidationIncomplete),
            "{}: an exact-tree delivery stays uncovered",
            case.name
        );
    }
}

/// The accepting shapes the refusals above are measured against: the same
/// command rerun, or a corrected command sharing the attempt's check identity.
/// A `check` only one record carries never stops the same command from
/// relating them [ORB-13894: a final `make ci-fast` pass without `check`
/// settled a passing review as incomplete].
#[test]
fn a_superseded_attempt_replaced_by_the_same_check_is_coverage() {
    for (name, records) in [
        (
            "same command rerun",
            vec![superseded(ATTEMPT, None), required(ATTEMPT, None, true)],
        ),
        (
            "same command rerun whose replacement omits the attempt's check",
            vec![
                superseded(ATTEMPT, Some("orbit-core-tests")),
                required(ATTEMPT, None, true),
            ],
        ),
        (
            "same command rerun with an empty check and different spacing",
            vec![
                superseded(ATTEMPT, Some("")),
                required(&format!("  {}", ATTEMPT.replace(' ', "  ")), None, true),
            ],
        ),
        (
            "corrected command with a shared check identity",
            vec![
                superseded(ATTEMPT, Some("orbit-core-tests")),
                required(CORRECTED, Some(" orbit-core-tests "), true),
            ],
        ),
    ] {
        assert_eq!(validation_evidence(&records), Ok(()), "{name}");
        let certificate = certificate(records);
        assert_eq!(certificate_acceptable(&certificate), Ok(()), "{name}");
        let covered = exclusion(&exact_delivery(), &certificate, &facts())
            .unwrap_or_else(|reason| panic!("{name}: {reason:?}"));
        assert_eq!(covered.final_candidate_tree, "tree-final", "{name}");
    }
}

/// Without any replacement question, a required check that did not pass and
/// a set with no required pass are refused at both boundaries too.
#[test]
fn a_set_without_a_passing_required_check_is_never_coverage() {
    for (name, records) in [
        ("empty", Vec::new()),
        (
            "required check failed",
            vec![required("make ci-fast", None, false)],
        ),
        (
            "only an explained negative control",
            vec![record(
                "grep -q legacy-header old.conf",
                None,
                ValidationOutcome::Failed,
                ValidationRole::ExpectedFailure,
                Some("the superseded assertion must no longer hold"),
            )],
        ),
    ] {
        assert!(validation_evidence(&records).is_err(), "{name}");
        assert_eq!(
            certificate_acceptable(&certificate(records)),
            Err(ReviewInvalidation::ValidationIncomplete),
            "{name}"
        );
    }
}

fn record(
    command: &str,
    check: Option<&str>,
    outcome: ValidationOutcome,
    role: ValidationRole,
    note: Option<&str>,
) -> ReviewValidation {
    ReviewValidation {
        command: command.to_string(),
        outcome,
        role,
        note: note.map(ToOwned::to_owned),
        check: check.map(ToOwned::to_owned),
    }
}

fn superseded(command: &str, check: Option<&str>) -> ReviewValidation {
    record(
        command,
        check,
        ValidationOutcome::Failed,
        ValidationRole::Superseded,
        Some("sandbox allowlist leak; rerun after the fix"),
    )
}

fn required(command: &str, check: Option<&str>, passed: bool) -> ReviewValidation {
    let outcome = if passed {
        ValidationOutcome::Passed
    } else {
        ValidationOutcome::Failed
    };
    record(command, check, outcome, ValidationRole::Required, None)
}

fn revision(label: &str) -> SourceRevision {
    SourceRevision {
        commit: format!("commit-{label}"),
        tree: format!("tree-{label}"),
    }
}

/// A passing certificate that claims complete validation over `validation`.
fn certificate(validation: Vec<ReviewValidation>) -> ReviewCertificate {
    let verdict = ReviewVerdict::PassedWithoutRepairs;
    ReviewCertificate {
        schema_version: REVIEW_CONTRACT_VERSION,
        attempt_id: "rvw-1".into(),
        lineage_key: "ws/task/agent-main".into(),
        task_ids: vec!["task".into()],
        task_meaning_digest: "meaning".into(),
        repository: "owner/repo".into(),
        base: revision("base"),
        reviewed_candidate: revision("final"),
        final_candidate: revision("final"),
        implementation_commits: Vec::new(),
        repair_commits: Vec::new(),
        verdict,
        assurance: verdict.assurance(),
        findings: Vec::new(),
        validation,
        validation_complete: true,
        reviewer: ReviewerIdentity {
            crew: "reviewers".into(),
            provider: "codex".into(),
            model: "reviewer-model".into(),
            reasoning_effort: None,
            implementer_model: Some("implementer-model".into()),
            same_model_as_implementer: false,
        },
        consumed: ReviewConsumption::default(),
        budget: ReviewBudget::default(),
        escalation: None,
        selectors_widened: Vec::new(),
        issued_at: Utc.with_ymd_and_hms(2026, 10, 3, 0, 0, 0).unwrap(),
    }
}

/// A delivery that reproduced the reviewed base and final trees exactly.
fn exact_delivery() -> Delivery {
    Delivery {
        key: "pr:owner/repo:agent-main:7".into(),
        repository: "owner/repo".into(),
        branch: "agent-main".into(),
        before: revision("base"),
        after: revision("final"),
        commits: vec!["landed".into()],
        task_ids: Vec::new(),
        evidence_reference: "https://github.com/owner/repo/pull/7".into(),
        evidence_digest: "digest".into(),
        landed_at: Utc.with_ymd_and_hms(2026, 10, 3, 1, 0, 0).unwrap(),
    }
}

fn facts() -> LandingFacts {
    LandingFacts {
        objects_present: true,
        task_meaning_current: true,
        managed_landing: None,
    }
}
