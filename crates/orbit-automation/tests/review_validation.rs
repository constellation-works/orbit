#![allow(missing_docs, clippy::expect_used, clippy::unwrap_used)]

//! The review validation gate through the public review-coverage boundary.
//!
//! A reviewer may file a failed attempt as `superseded`. The attempt then
//! counts only when a later required check that passed replaced it: the same
//! effective identity: a non-empty `check`, otherwise the normalized command. Both
//! consumers of these rules are driven here. [`validation_evidence`] gives the
//! reason the issuing gate escalates with. [`certificate_acceptable`] and
//! [`exclusion`] decide whether an issued certificate covers a delivery. A
//! certificate whose `validation_complete` flag is set but whose records do
//! not establish the replacement must never be spent as coverage.

use chrono::{TimeZone, Utc};
use orbit_automation::review::{
    LandingFacts, ValidationContext, ValidationDefect, certificate_acceptable, exclusion,
    validation_evidence, validation_limitations,
};
use orbit_types::workflow::automation::{Delivery, SourceRevision};
use orbit_types::workflow::{
    NegativeControl, REVIEW_CONTRACT_VERSION, RetainedObligation, ReviewBudget, ReviewCertificate,
    ReviewConsumption, ReviewInvalidation, ReviewValidation, ReviewVerdict, ReviewerIdentity,
    ValidationOutcome, ValidationRole,
};

const ATTEMPT: &str = "cargo test --package orbit-core";
/// The workspace-wide diagnostic ORB-14151's reviewer ran on its final
/// candidate; it failed only in engine fixtures the task never touched.
const WORKSPACE: &str = "cargo test --workspace --no-fail-fast";
const UNRELATED_FIXTURE: &str = "crates/orbit-engine/tests/fixtures/f066_resume.rs";
/// ORB-14191's required CodeQL run, which its replacement report omitted.
const CODEQL: &str = "scripts/codeql-rust-local.sh --ram 16384 \
     codeql/rust-queries:codeql-suites/rust-security-extended.qls";

/// The candidate's scope: its task selectors and every path it changed.
fn scope() -> Vec<String> {
    vec![
        "dir:crates/orbit-review".to_string(),
        "file:crates/orbit-review/src/fix.rs".to_string(),
        "file:crates/orbit-review/tests/fix.rs".to_string(),
    ]
}
/// Differs from [`ATTEMPT`] by an argument. A leading environment assignment
/// is the same command, not this fixture [ORB-14302].
const CORRECTED: &str = "cargo test --package orbit-core --locked";

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
            name: "missing: an explicit identity names a different command",
            records: vec![
                superseded(ATTEMPT, None),
                required(CORRECTED, Some("cargo test --package orbit-types"), true),
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
        let defect = validation_evidence(&case.records, &ValidationContext::default())
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
/// effective identity, even when only one record explicitly supplies it
/// [ORB-13894: a final `make ci-fast` pass without `check` settled a passing
/// review as incomplete].
#[test]
fn a_superseded_attempt_replaced_by_the_same_check_is_coverage() {
    for (name, records) in [
        (
            "same command rerun",
            vec![superseded(ATTEMPT, None), required(ATTEMPT, None, true)],
        ),
        (
            "replacement command matches the attempt's explicit identity",
            vec![
                superseded(ATTEMPT, Some(ATTEMPT)),
                required(ATTEMPT, None, true),
            ],
        ),
        (
            "replacement identity matches the attempt's command",
            vec![
                superseded(ATTEMPT, None),
                required(CORRECTED, Some(ATTEMPT), true),
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
        (
            "leading environment assignment is the same command",
            vec![
                superseded(ATTEMPT, None),
                required(
                    "ORBIT_TEST_ALLOWLIST=1 cargo test --package orbit-core",
                    None,
                    true,
                ),
            ],
        ),
    ] {
        assert_eq!(
            validation_evidence(&records, &ValidationContext::default()),
            Ok(()),
            "{name}"
        );
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
        assert!(
            validation_evidence(&records, &ValidationContext::default()).is_err(),
            "{name}"
        );
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
        control: None,
        sources: Vec::new(),
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

/// A passing certificate that claims complete validation over `validation`,
/// judged against [`SCOPE`] with no retained obligations.
fn certificate(validation: Vec<ReviewValidation>) -> ReviewCertificate {
    certificate_with(validation, &scope(), Vec::new())
}

/// A passing certificate claiming complete validation over `validation`,
/// recording the scope and retained obligations it was issued against.
fn certificate_with(
    validation: Vec<ReviewValidation>,
    validation_scope: &[String],
    retained_obligations: Vec<RetainedObligation>,
) -> ReviewCertificate {
    let verdict = ReviewVerdict::Accept;
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
        required_validation_commands: Some(vec![]),
        validation_complete: true,
        retained_obligations,
        validation_scope: validation_scope.to_vec(),
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
        unattributed: None,
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

fn diagnostic(command: &str, outcome: ValidationOutcome, sources: &[&str]) -> ReviewValidation {
    let mut record = record(
        command,
        None,
        outcome,
        ValidationRole::Diagnostic,
        Some("workspace-wide observation outside the task's scope"),
    );
    record.sources = sources.iter().map(|source| (*source).to_string()).collect();
    record
}

fn control(
    command: &str,
    kind: Option<NegativeControl>,
    outcome: ValidationOutcome,
    sources: &[&str],
) -> ReviewValidation {
    let mut record = record(
        command,
        None,
        outcome,
        ValidationRole::ExpectedFailure,
        Some("the regression the fix removes"),
    );
    record.control = kind;
    record.sources = sources.iter().map(|source| (*source).to_string()).collect();
    record
}

fn retained(validation: ReviewValidation) -> RetainedObligation {
    RetainedObligation {
        report_sha256: "first-report".into(),
        observed_at: Utc.with_ymd_and_hms(2026, 10, 5, 9, 54, 0).unwrap(),
        validation,
    }
}

/// Retained obligations match by effective identity across report revisions
/// [ORB-14312], while different commands and explicit identities stay distinct.
#[test]
fn retained_obligations_match_effective_identities_across_report_revisions() {
    const WRAPPED: &str = "set -o pipefail; make ci-fast 2>&1 | tee .orbit/tmp/review-ci-fast.log";
    const PREFIXED: &str = "TMPDIR=\"$PWD/.orbit/tmp\" FOO='two words' make  ci-fast";
    for (name, earlier_command, earlier_check, final_command, final_check, matches) in [
        (
            "command to wrapped explicit identity",
            "make ci-fast",
            None,
            WRAPPED,
            Some("make ci-fast"),
            true,
        ),
        (
            "wrapped explicit identity to command",
            WRAPPED,
            Some("make ci-fast"),
            "make ci-fast",
            None,
            true,
        ),
        (
            "normalized command to trimmed explicit identity",
            PREFIXED,
            None,
            WRAPPED,
            Some(" make ci-fast "),
            true,
        ),
        (
            "trimmed explicit identity to normalized command",
            WRAPPED,
            Some(" make ci-fast "),
            PREFIXED,
            None,
            true,
        ),
        (
            "different bare commands",
            "make ci-fast",
            None,
            "make ci-lint",
            None,
            false,
        ),
        (
            "different command suffix",
            "make ci-fast",
            None,
            "make ci-fast-extra",
            None,
            false,
        ),
        (
            "command differs from final explicit identity",
            "make ci-fast",
            None,
            WRAPPED,
            Some("make ci-lint"),
            false,
        ),
        (
            "earlier explicit identity differs from command",
            WRAPPED,
            Some("make ci-lint"),
            "make ci-fast",
            None,
            false,
        ),
        (
            "distinct explicit identities override identical commands",
            "make ci-fast",
            Some("fast-check"),
            "make ci-fast",
            Some("lint-check"),
            false,
        ),
        (
            "explicit identity overrides the earlier command",
            "make ci-fast",
            Some("fast-check"),
            "make ci-fast",
            None,
            false,
        ),
    ] {
        let obligations = vec![retained(record(
            earlier_command,
            earlier_check,
            ValidationOutcome::NotRun,
            ValidationRole::Required,
            None,
        ))];
        let records = vec![required(final_command, final_check, true)];
        let context = ValidationContext {
            obligations: &obligations,
            ..ValidationContext::default()
        };
        let expected = if matches {
            Ok(())
        } else {
            Err(ValidationDefect::ObligationDropped {
                command: earlier_command.into(),
                outcome: ValidationOutcome::NotRun,
                role: None,
            })
        };
        assert_eq!(
            validation_evidence(&records, &context),
            expected,
            "ORB-14312: {name}"
        );
        let certificate = certificate_with(records, &scope(), obligations);
        let expected_coverage = if matches {
            Ok(())
        } else {
            Err(ReviewInvalidation::ValidationIncomplete)
        };
        assert_eq!(
            certificate_acceptable(&certificate),
            expected_coverage,
            "ORB-14312: {name}: certificate coverage"
        );
        assert_eq!(
            exclusion(&exact_delivery(), &certificate, &facts()).map(|_| ()),
            expected_coverage,
            "ORB-14312: {name}: delivery coverage"
        );
    }
}

/// The task's nine scoped regressions, all passing on the final candidate.
fn scoped_passes() -> Vec<ReviewValidation> {
    (1..=9)
        .map(|index| {
            required(
                &format!("cargo test -p orbit-review --test fix case_{index}"),
                None,
                true,
            )
        })
        .collect()
}

/// ORB-14151's shape: every task-scoped regression passed and a final-candidate
/// workspace run failed only in unrelated fixtures. Recorded as diagnostics, the
/// failures keep their exact outcome and sources, the bounded task's validation
/// is complete, and the certificate says what it did not establish instead of
/// claiming the workspace passed.
#[test]
fn an_unrelated_workspace_failure_is_an_honest_diagnostic_not_a_blocker_or_a_control() {
    let mut records = scoped_passes();
    records.push(diagnostic(
        WORKSPACE,
        ValidationOutcome::Failed,
        &[
            UNRELATED_FIXTURE,
            "crates/orbit-engine/tests/fixtures/f071_claim.rs",
        ],
    ));
    records.push(diagnostic(
        "cargo test -p orbit-engine f066",
        ValidationOutcome::Failed,
        &[UNRELATED_FIXTURE],
    ));
    let scope = scope();
    let context = ValidationContext {
        scope: &scope,
        obligations: &[],
        required_validation_commands: Some(&[]),
    };
    assert_eq!(validation_evidence(&records, &context), Ok(()));

    let certificate = certificate(records.clone());
    assert_eq!(certificate_acceptable(&certificate), Ok(()));
    exclusion(&exact_delivery(), &certificate, &facts())
        .expect("the bounded task is covered without a workspace-green claim");
    assert_eq!(
        validation_limitations(&certificate.validation),
        vec![
            format!(
                "diagnostic `{WORKSPACE}` failed in {UNRELATED_FIXTURE}, \
                 crates/orbit-engine/tests/fixtures/f071_claim.rs"
            ),
            format!("diagnostic `cargo test -p orbit-engine f066` failed in {UNRELATED_FIXTURE}"),
        ],
        "the certificate discloses exactly what it does not establish"
    );
    assert!(
        certificate
            .validation
            .iter()
            .filter(|record| record.role == ValidationRole::Diagnostic)
            .all(|record| record.outcome == ValidationOutcome::Failed),
        "raw failed outcomes are kept, never rewritten as passes"
    );

    // The same observations filed as expected failures, as ORB-14151's
    // report did, are not deliberate controls and never pass.
    let mut mislabeled = scoped_passes();
    mislabeled.push(record(
        WORKSPACE,
        None,
        ValidationOutcome::Failed,
        ValidationRole::ExpectedFailure,
        Some("out of scope for this task"),
    ));
    assert_eq!(
        validation_evidence(&mislabeled, &context),
        Err(ValidationDefect::ClassificationUnevidenced {
            command: WORKSPACE.into(),
            role: ValidationRole::ExpectedFailure,
            missing: "control",
        })
    );
    assert_eq!(
        certificate_acceptable(&certificate_with(mislabeled, &scope, Vec::new())),
        Err(ReviewInvalidation::ValidationIncomplete)
    );
}

/// A required check that failed, was denied, never ran, or was dropped cannot
/// be cleared by changing its role or note, beside unrelated passing checks,
/// at issuance or when a certificate is spent.
#[test]
fn relabeling_or_dropping_a_required_check_never_completes_validation() {
    let codeql_failed = required(CODEQL, None, false);
    let in_scope = "crates/orbit-review/src/fix.rs";
    let cases: Vec<(
        &str,
        Vec<ReviewValidation>,
        Vec<RetainedObligation>,
        ValidationDefect,
    )> = vec![
        (
            "a failed check relabeled expected_failure with only a note",
            vec![record(
                "make ci-fast",
                None,
                ValidationOutcome::Failed,
                ValidationRole::ExpectedFailure,
                Some("known flake"),
            )],
            Vec::new(),
            ValidationDefect::ClassificationUnevidenced {
                command: "make ci-fast".into(),
                role: ValidationRole::ExpectedFailure,
                missing: "control",
            },
        ),
        (
            "a control naming code outside the candidate's scope",
            vec![control(
                "make ci-fast",
                Some(NegativeControl::Counterfactual),
                ValidationOutcome::Failed,
                &[UNRELATED_FIXTURE],
            )],
            Vec::new(),
            ValidationDefect::ControlOutOfScope {
                command: "make ci-fast".into(),
                source: UNRELATED_FIXTURE.into(),
            },
        ),
        (
            "a failed check relabeled diagnostic with only a note",
            vec![diagnostic("make ci-fast", ValidationOutcome::Failed, &[])],
            Vec::new(),
            ValidationDefect::ClassificationUnevidenced {
                command: "make ci-fast".into(),
                role: ValidationRole::Diagnostic,
                missing: "sources",
            },
        ),
        (
            "a diagnostic whose failure lies in the candidate's own code",
            vec![diagnostic(
                "make ci-fast",
                ValidationOutcome::Failed,
                &[UNRELATED_FIXTURE, in_scope],
            )],
            Vec::new(),
            ValidationDefect::DiagnosticInScope {
                command: "make ci-fast".into(),
                source: in_scope.into(),
            },
        ),
        (
            "a diagnostic failing the same check a required record passed",
            vec![
                diagnostic(WORKSPACE, ValidationOutcome::Failed, &[UNRELATED_FIXTURE]),
                required(WORKSPACE, None, true),
            ],
            Vec::new(),
            ValidationDefect::CheckContradicted {
                command: WORKSPACE.into(),
                role: ValidationRole::Diagnostic,
            },
        ),
        (
            "a denied check relabeled diagnostic",
            vec![diagnostic(CODEQL, ValidationOutcome::Denied, &[])],
            Vec::new(),
            ValidationDefect::RoleContradicted {
                command: CODEQL.into(),
                role: ValidationRole::Diagnostic,
                outcome: ValidationOutcome::Denied,
            },
        ),
        (
            "a retained required failure the final report omits (ORB-14191)",
            Vec::new(),
            vec![retained(codeql_failed.clone())],
            ValidationDefect::ObligationDropped {
                command: CODEQL.into(),
                outcome: ValidationOutcome::Failed,
                role: None,
            },
        ),
        (
            "a retained required failure the final report calls a diagnostic",
            vec![diagnostic(
                CODEQL,
                ValidationOutcome::Failed,
                &[UNRELATED_FIXTURE],
            )],
            vec![retained(codeql_failed.clone())],
            ValidationDefect::ObligationDropped {
                command: CODEQL.into(),
                outcome: ValidationOutcome::Failed,
                role: Some(ValidationRole::Diagnostic),
            },
        ),
        (
            "a retained required failure the final report calls a control",
            vec![control(
                CODEQL,
                Some(NegativeControl::PreFix),
                ValidationOutcome::Failed,
                &[in_scope],
            )],
            vec![retained(codeql_failed.clone())],
            ValidationDefect::ObligationDropped {
                command: CODEQL.into(),
                outcome: ValidationOutcome::Failed,
                role: Some(ValidationRole::ExpectedFailure),
            },
        ),
        (
            "a retained required failure the final report calls excluded",
            vec![record(
                CODEQL,
                None,
                ValidationOutcome::NotRun,
                ValidationRole::Excluded,
                Some("not needed"),
            )],
            vec![retained(codeql_failed)],
            ValidationDefect::ObligationDropped {
                command: CODEQL.into(),
                outcome: ValidationOutcome::Failed,
                role: Some(ValidationRole::Excluded),
            },
        ),
    ];

    for (name, mut records, obligations, expected) in cases {
        // Unrelated passing checks never pay for the defect.
        records.extend(scoped_passes());
        let scope = scope();
        let context = ValidationContext {
            scope: &scope,
            obligations: &obligations,
            required_validation_commands: Some(&[]),
        };
        assert_eq!(
            validation_evidence(&records, &context),
            Err(expected),
            "{name}"
        );
        let certificate = certificate_with(records, &scope, obligations);
        assert_eq!(
            certificate_acceptable(&certificate),
            Err(ReviewInvalidation::ValidationIncomplete),
            "{name}: a flagged certificate is not coverage"
        );
        assert_eq!(
            exclusion(&exact_delivery(), &certificate, &facts()).map(|_| ()),
            Err(ReviewInvalidation::ValidationIncomplete),
            "{name}"
        );
    }

    // A failed diagnostic judged with no recorded scope proves nothing about
    // where its failures lie.
    let mut unscoped = scoped_passes();
    unscoped.push(diagnostic(
        WORKSPACE,
        ValidationOutcome::Failed,
        &[UNRELATED_FIXTURE],
    ));
    assert_eq!(
        validation_evidence(&unscoped, &ValidationContext::default()),
        Err(ValidationDefect::ScopeUnknown {
            command: WORKSPACE.into()
        })
    );
    assert_eq!(
        certificate_acceptable(&certificate_with(unscoped, &[], Vec::new())),
        Err(ReviewInvalidation::ValidationIncomplete)
    );
}

/// Deliberate controls, retained obligations a later record legitimately
/// resolves, and ORB-11528's exclusion and supersession stay coverage; a must-
/// fail control that passed, or one run on the candidate that a required pass
/// of the same check contradicts, does not.
#[test]
fn deliberate_controls_and_resolved_obligations_remain_coverage() {
    let test = "cargo test -p orbit-review --test fix regression";
    let source = "crates/orbit-review/tests/fix.rs";
    let accepted: Vec<(&str, Vec<ReviewValidation>, Vec<RetainedObligation>)> = vec![
        (
            "a pre-fix reproduction of the check that now passes",
            vec![
                control(
                    test,
                    Some(NegativeControl::PreFix),
                    ValidationOutcome::Failed,
                    &[source],
                ),
                required(test, None, true),
            ],
            Vec::new(),
        ),
        (
            "a counterfactual run on the candidate",
            vec![
                control(
                    "cargo test -p orbit-review --test fix rejects_mutation",
                    Some(NegativeControl::Counterfactual),
                    ValidationOutcome::Failed,
                    &["dir:crates/orbit-review"],
                ),
                required(test, None, true),
            ],
            Vec::new(),
        ),
        (
            "a retained failure rerun and passed",
            vec![required(CODEQL, None, true)],
            vec![retained(required(CODEQL, None, false))],
        ),
        (
            "a retained failure superseded and replaced by the same check",
            vec![
                superseded(CODEQL, Some(CODEQL)),
                required("ORBIT_RAM=1 codeql", Some(CODEQL), true),
            ],
            vec![retained(required(CODEQL, None, false))],
        ),
        (
            "a retained unperformed action later excluded as out of scope",
            vec![
                record(
                    "orbit deploy production",
                    None,
                    ValidationOutcome::NotRun,
                    ValidationRole::Excluded,
                    Some("deployment is not authorized by this task"),
                ),
                required(test, None, true),
            ],
            vec![retained(record(
                "orbit deploy production",
                None,
                ValidationOutcome::NotRun,
                ValidationRole::Required,
                None,
            ))],
        ),
        (
            "role-less legacy records read as required checks",
            vec![required(test, None, true)],
            Vec::new(),
        ),
    ];
    for (name, records, obligations) in accepted {
        let scope = scope();
        let context = ValidationContext {
            scope: &scope,
            obligations: &obligations,
            required_validation_commands: Some(&[]),
        };
        assert_eq!(validation_evidence(&records, &context), Ok(()), "{name}");
        assert_eq!(
            certificate_acceptable(&certificate_with(records, &scope, obligations)),
            Ok(()),
            "{name}"
        );
    }

    let refused = [
        (
            "a must-fail control that passed",
            vec![
                control(
                    test,
                    Some(NegativeControl::PreFix),
                    ValidationOutcome::Passed,
                    &[source],
                ),
                required(test, None, true),
            ],
            ValidationDefect::RoleContradicted {
                command: test.into(),
                role: ValidationRole::ExpectedFailure,
                outcome: ValidationOutcome::Passed,
            },
        ),
        (
            "a candidate-side control the same required pass contradicts",
            vec![
                control(
                    test,
                    Some(NegativeControl::SupersededAssertion),
                    ValidationOutcome::Failed,
                    &[source],
                ),
                required(test, None, true),
            ],
            ValidationDefect::CheckContradicted {
                command: test.into(),
                role: ValidationRole::ExpectedFailure,
            },
        ),
        (
            "a control naming no sources",
            vec![
                control(
                    test,
                    Some(NegativeControl::PreFix),
                    ValidationOutcome::Failed,
                    &[],
                ),
                required(test, None, true),
            ],
            ValidationDefect::ClassificationUnevidenced {
                command: test.into(),
                role: ValidationRole::ExpectedFailure,
                missing: "sources",
            },
        ),
    ];
    for (name, records, expected) in refused {
        let scope = scope();
        let context = ValidationContext {
            scope: &scope,
            obligations: &[],
            required_validation_commands: Some(&[]),
        };
        assert_eq!(
            validation_evidence(&records, &context),
            Err(expected),
            "{name}"
        );
        assert_eq!(
            certificate_acceptable(&certificate_with(records, &scope, Vec::new())),
            Err(ReviewInvalidation::ValidationIncomplete),
            "{name}"
        );
    }
}

#[test]
fn captured_host_checks_cannot_be_omitted_or_reclassified() {
    let command = "make ci-fast";
    let host_required = vec![command.to_string()];
    let task_scope = scope();
    let context = ValidationContext {
        scope: &task_scope,
        obligations: &[],
        required_validation_commands: Some(&host_required),
    };
    let cases = [
        (
            "missing",
            scoped_passes(),
            ValidationDefect::HostCheckNotEstablished {
                command: command.into(),
            },
        ),
        (
            "failed",
            {
                let mut records = scoped_passes();
                records.push(record(
                    command,
                    None,
                    ValidationOutcome::Failed,
                    ValidationRole::Required,
                    None,
                ));
                records
            },
            ValidationDefect::RequiredNotPassed {
                command: command.into(),
                outcome: ValidationOutcome::Failed,
            },
        ),
        (
            "denied",
            {
                let mut records = scoped_passes();
                records.push(record(
                    command,
                    None,
                    ValidationOutcome::Denied,
                    ValidationRole::Required,
                    None,
                ));
                records
            },
            ValidationDefect::RequiredNotPassed {
                command: command.into(),
                outcome: ValidationOutcome::Denied,
            },
        ),
        (
            "diagnostic substitution",
            {
                let mut records = scoped_passes();
                records.push(diagnostic(
                    command,
                    ValidationOutcome::Failed,
                    &[UNRELATED_FIXTURE],
                ));
                records
            },
            ValidationDefect::HostCheckNotEstablished {
                command: command.into(),
            },
        ),
        (
            "negative-control substitution",
            {
                let mut records = scoped_passes();
                records.push(control(
                    command,
                    Some(NegativeControl::PreFix),
                    ValidationOutcome::Failed,
                    &["crates/orbit-review/src/fix.rs"],
                ));
                records
            },
            ValidationDefect::HostCheckNotEstablished {
                command: command.into(),
            },
        ),
        (
            "excluded substitution",
            {
                let mut records = scoped_passes();
                records.push(record(
                    command,
                    None,
                    ValidationOutcome::NotRun,
                    ValidationRole::Excluded,
                    Some("the check was not available"),
                ));
                records
            },
            ValidationDefect::HostCheckNotEstablished {
                command: command.into(),
            },
        ),
    ];

    for (name, records, expected) in cases {
        assert_eq!(
            validation_evidence(&records, &context),
            Err(expected),
            "{name}"
        );
        let mut certificate = certificate_with(records, &task_scope, Vec::new());
        certificate.required_validation_commands = Some(host_required.clone());
        assert_eq!(
            certificate_acceptable(&certificate),
            Err(ReviewInvalidation::ValidationIncomplete),
            "consumer refuses {name}"
        );
    }

    let mut valid = scoped_passes();
    valid.push(required(command, None, true));
    assert_eq!(validation_evidence(&valid, &context), Ok(()));

    let mut legacy = certificate(scoped_passes());
    legacy.required_validation_commands = None;
    assert_eq!(
        certificate_acceptable(&legacy),
        Err(ReviewInvalidation::ValidationContractMissing)
    );
}

/// ORB-14302: reviewers prefix `TMPDIR` because a nested temp directory is
/// not hermetic. That required pass is the host command. A different
/// command is not, including one that only grows the name or changes the
/// program after its own leading assignment.
#[test]
fn a_leading_env_assignment_satisfies_the_host_required_command() {
    let host = "make ci-fast";
    let host_required = vec![host.to_string()];
    let task_scope = scope();
    let context = ValidationContext {
        scope: &task_scope,
        obligations: &[],
        required_validation_commands: Some(&host_required),
    };
    let satisfied = [
        r#"TMPDIR="$PWD/.orbit/tmp" make ci-fast"#,
        "TMPDIR='$PWD/.orbit/tmp' make ci-fast",
        "TMPDIR=$PWD/.orbit/tmp make ci-fast",
        r#"FOO=1 TMPDIR="$PWD/.orbit/tmp" make ci-fast"#,
        r#"TMPDIR="/tmp/orbit tmp" make  ci-fast"#,
    ];
    for command in satisfied {
        let records = [required(command, None, true)];
        assert_eq!(validation_evidence(&records, &context), Ok(()), "{command}");
        let mut certificate = certificate_with(records.to_vec(), &task_scope, Vec::new());
        certificate.required_validation_commands = Some(host_required.clone());
        assert_eq!(certificate_acceptable(&certificate), Ok(()), "{command}");
    }

    let wrapped = [required(
        r#"env TMPDIR="$PWD/.orbit/tmp" make ci-fast"#,
        Some(host),
        true,
    )];
    assert_eq!(
        validation_evidence(&wrapped, &context),
        Ok(()),
        "a non-assignment wrap satisfies the host command when check names it"
    );
    let wrapped_without_check = [required(
        r#"env TMPDIR="$PWD/.orbit/tmp" make ci-fast"#,
        None,
        true,
    )];
    assert_eq!(
        validation_evidence(&wrapped_without_check, &context),
        Err(ValidationDefect::HostCheckNotEstablished {
            command: host.into(),
        }),
        "a non-assignment wrap without check is a different command"
    );

    let refused = [
        "make ci-fast-extra",
        "FOO=1 make other",
        r#"TMPDIR="$PWD/.orbit/tmp" make ci-fast-extra"#,
        "make TMPDIR=1 ci-fast",
        r#""TMPDIR=1" make ci-fast"#,
    ];
    for command in refused {
        let records = [required(command, None, true)];
        assert_eq!(
            validation_evidence(&records, &context),
            Err(ValidationDefect::HostCheckNotEstablished {
                command: host.into(),
            }),
            "{command}"
        );
    }
}

#[test]
fn captured_host_check_can_be_resolved_by_a_valid_same_check_replacement() {
    let command = "make ci-fast";
    let required_commands = vec![command.to_string()];
    let task_scope = scope();
    let context = ValidationContext {
        scope: &task_scope,
        obligations: &[],
        required_validation_commands: Some(&required_commands),
    };
    let records = vec![
        record(
            command,
            Some("host-ci"),
            ValidationOutcome::Failed,
            ValidationRole::Superseded,
            Some("the runner environment was corrected"),
        ),
        required("make ci-fast --locked", Some("host-ci"), true),
    ];
    assert_eq!(validation_evidence(&records, &context), Ok(()));
}

#[test]
fn missing_host_snapshot_is_not_an_empty_requirement_list() {
    let task_scope = scope();
    let context = ValidationContext {
        scope: &task_scope,
        obligations: &[],
        required_validation_commands: None,
    };
    assert_eq!(
        validation_evidence(&scoped_passes(), &context),
        Err(ValidationDefect::HostContractMissing)
    );
}
