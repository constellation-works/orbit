#![allow(missing_docs, clippy::expect_used, clippy::unwrap_used)]

//! The review validation gate through the public review-coverage boundary.
//!
//! A reviewer may file a failed attempt as `superseded`. The attempt then
//! counts only when a required check that passed replaced it, in either report
//! order, with the same effective identity: a non-empty `check`, otherwise the
//! normalized command. Both
//! consumers of these rules are driven here. [`validation_evidence`] gives the
//! reason the issuing gate escalates with. [`certificate_acceptable`] and
//! [`exclusion`] decide whether an issued certificate covers a delivery. A
//! certificate whose `validation_complete` flag is set but whose records do
//! not establish the replacement must never be spent as coverage.

use chrono::{TimeZone, Utc};
use orbit_automation::review::{
    LandingFacts, ValidationContext, ValidationDefect, certificate_acceptable, exclusion,
    mutation_targets, validation_evidence, validation_limitations,
};
use orbit_types::workflow::automation::{Delivery, SourceRevision};
use orbit_types::workflow::{
    NegativeControl, REVIEW_CONTRACT_VERSION, RecordGap, RetainedObligation, RetiredValidation,
    ReviewBudget, ReviewCertificate, ReviewConsumption, ReviewInvalidation, ReviewValidation,
    ReviewVerdict, ReviewerIdentity, ValidationOutcome, ValidationRole,
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
            name: "missing: no same-identity required pass anywhere",
            records: vec![
                required("make ci-lint", None, true),
                superseded(ATTEMPT, None),
                required("cargo fmt --check", None, true),
            ],
        },
        Case {
            name: "invalid: a broader check identity with the same command",
            records: vec![
                required(ATTEMPT, Some("runtime tests and formatting"), true),
                superseded(ATTEMPT, Some("runtime tests")),
                required(ATTEMPT, Some("runtime tests and formatting"), true),
            ],
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
    for (name, mut records) in [
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
        // ORB-14322: final-candidate required passes replace the same check
        // regardless of report order, at issuance and coverage consumption.
        for order in ["attempt first", "pass first"] {
            assert_eq!(
                validation_evidence(&records, &ValidationContext::default()),
                Ok(()),
                "{name}, {order}"
            );
            let certificate = certificate(records.clone());
            assert_eq!(
                certificate_acceptable(&certificate),
                Ok(()),
                "{name}, {order}"
            );
            let covered = exclusion(&exact_delivery(), &certificate, &facts())
                .unwrap_or_else(|reason| panic!("{name}, {order}: {reason:?}"));
            assert_eq!(
                covered.final_candidate_tree, "tree-final",
                "{name}, {order}"
            );
            records.reverse();
        }
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
        id: None,
        command: command.to_string(),
        outcome,
        role,
        note: note.map(ToOwned::to_owned),
        check: check.map(ToOwned::to_owned),
        control: None,
        sources: Vec::new(),
        mutation_target: Vec::new(),
        baseline: None,
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
        baseline_commands: Vec::new(),
        validation_complete: true,
        retained_obligations,
        retired_validation: Vec::new(),
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
        evidence_carried: None,
        baseline_red: Vec::new(),
        host_evidence: Vec::new(),
        issued_at: Utc.with_ymd_and_hms(2026, 10, 3, 0, 0, 0).unwrap(),
        owed_evidence: Vec::new(),
        resumed_hold_attempt: None,
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
            retired: &[],
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

fn with_id(mut record: ReviewValidation, id: &str) -> ReviewValidation {
    record.id = Some(id.into());
    record
}

fn retire(id: &str, reason: &str) -> RetiredValidation {
    RetiredValidation {
        id: id.into(),
        reason: reason.into(),
    }
}

/// An earlier record with an id is accounted for by that id alone
/// [ORB-14370]: carrying it forward settles whatever the command now reads
/// (ORB-14360's prose name, ORB-14260's `<workspace>` placeholder), while
/// omitting, relabeling or improperly retiring it is a dropped record even
/// when another record runs the identical command. Settlement and coverage
/// agree on every case.
#[test]
fn record_ids_account_for_earlier_records_regardless_of_command_text() {
    const PROSE: &str = "focused CLI reference invocation verification";
    const SCRIPT: &str = "python3 .orbit/tmp/verify-reference-examples.py";
    const PLACEHOLDER: &str = "cargo test --manifest-path <workspace>/Cargo.toml";
    const ABSOLUTE: &str = "cargo test --manifest-path /srv/checkout/Cargo.toml";
    let not_run = |command: &str| {
        with_id(
            record(
                command,
                None,
                ValidationOutcome::NotRun,
                ValidationRole::Required,
                None,
            ),
            "V1",
        )
    };
    let failed = |command: &str| with_id(required(command, None, false), "V1");
    let passed = |command: &str, id: &str| with_id(required(command, None, true), id);
    let gap = |earlier: &ReviewValidation, gap: RecordGap| {
        Err(ValidationDefect::RecordDropped {
            id: "V1".into(),
            command: earlier.command.clone(),
            outcome: earlier.outcome,
            gap,
        })
    };
    type Case = (
        &'static str,
        ReviewValidation,
        Vec<ReviewValidation>,
        Vec<RetiredValidation>,
        Result<(), ValidationDefect>,
    );
    let cases: Vec<Case> = vec![
        (
            "ORB-14360: prose name to the real command",
            not_run(PROSE),
            vec![passed(SCRIPT, "V1")],
            vec![],
            Ok(()),
        ),
        (
            "ORB-14260: placeholder to the absolute path",
            not_run(PLACEHOLDER),
            vec![passed(ABSOLUTE, "V1")],
            vec![],
            Ok(()),
        ),
        (
            "a failed record rerun under its id with a new command",
            failed("make ci-fast"),
            vec![
                with_id(superseded("make ci-fast", None), "V1"),
                passed("set -o pipefail; make ci-fast | tee log", "V1"),
            ],
            vec![],
            Ok(()),
        ),
        (
            "an unrun record retired with a reason",
            not_run(PROSE),
            vec![passed(SCRIPT, "V2")],
            vec![retire("V1", "folded into V2")],
            Ok(()),
        ),
        (
            "the same command under another id",
            not_run(SCRIPT),
            vec![passed(SCRIPT, "V2")],
            vec![],
            gap(&not_run(SCRIPT), RecordGap::Omitted),
        ),
        (
            "the id carried only as a diagnostic",
            not_run(PROSE),
            vec![
                passed("make ci-lint", "V2"),
                with_id(diagnostic(SCRIPT, ValidationOutcome::Passed, &[]), "V1"),
            ],
            vec![],
            gap(
                &not_run(PROSE),
                RecordGap::Reclassified(ValidationRole::Diagnostic),
            ),
        ),
        (
            "a retirement without a reason",
            not_run(PROSE),
            vec![passed(SCRIPT, "V2")],
            vec![retire("V1", "  ")],
            gap(&not_run(PROSE), RecordGap::RetirementUnexplained),
        ),
        (
            "a failed record retired",
            failed("make ci-fast"),
            vec![passed("make ci-lint", "V2")],
            vec![retire("V1", "flaky")],
            gap(&failed("make ci-fast"), RecordGap::FailureRetired),
        ),
    ];
    for (name, earlier, records, retired, expected) in cases {
        let obligations = vec![retained(earlier)];
        let context = ValidationContext {
            obligations: &obligations,
            retired: &retired,
            ..ValidationContext::default()
        };
        assert_eq!(validation_evidence(&records, &context), expected, "{name}");
        let mut certificate = certificate_with(records, &scope(), obligations);
        certificate.retired_validation = retired;
        assert_eq!(
            certificate_acceptable(&certificate).is_ok(),
            expected.is_ok(),
            "{name}: certificate coverage"
        );
    }
}

/// A certificate issued before record ids, stored without `id` or
/// `retired_validation`, is read back and spent under the command-identity
/// rules it was issued under.
#[test]
fn certificates_without_record_ids_keep_their_coverage() {
    let obligations = vec![retained(record(
        "make ci-fast",
        None,
        ValidationOutcome::NotRun,
        ValidationRole::Required,
        None,
    ))];
    for (current, covered) in [
        ("TMPDIR=\"$PWD/.orbit/tmp\" make ci-fast", true),
        ("make ci-lint", false),
    ] {
        let issued = certificate_with(
            vec![required(current, None, true)],
            &scope(),
            obligations.clone(),
        );
        let stored = serde_json::to_value(&issued).unwrap();
        assert!(stored.get("retired_validation").is_none());
        assert!(stored["validation"][0].get("id").is_none());
        let certificate: ReviewCertificate = serde_json::from_value(stored).unwrap();
        assert_eq!(
            certificate_acceptable(&certificate).is_ok(),
            covered,
            "{current}"
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
        retired: &[],
        required_validation_commands: Some(&[]),
        baseline_commands: &[],
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

/// [ORB-14616] ORB-14521's shape: a test-only candidate whose reviewer proved
/// the repaired test guards a production check by deleting a conjunct from
/// that out-of-scope file, watching the test fail and restoring it. The
/// mutated file is the control's `mutation_target`, judged only for its
/// restoration; the checks that rejected the mutation stay its in-scope
/// `sources`. Naming the mutated file as a source is a shape defect the
/// reviewer is asked to correct, and a target the review left changed is
/// refused naming the file.
#[test]
fn a_counterfactual_names_the_file_it_mutated_apart_from_its_in_scope_sources() {
    let production = "crates/orbit-engine/src/converge.rs";
    let mutation = |sources: &[&str], targets: &[&str]| {
        let mut record = control(
            "cargo test -p orbit-review --test fix guards_read_only",
            Some(NegativeControl::Counterfactual),
            ValidationOutcome::Failed,
            sources,
        );
        record.mutation_target = targets.iter().map(|target| (*target).to_string()).collect();
        let mut records = scoped_passes();
        records.push(record);
        records
    };
    let scope = scope();
    let context = ValidationContext {
        scope: &scope,
        obligations: &[],
        retired: &[],
        required_validation_commands: Some(&[]),
        baseline_commands: &[],
    };

    let restored = mutation(&["crates/orbit-review/tests/fix.rs"], &[production]);
    assert_eq!(validation_evidence(&restored, &context), Ok(()));
    assert_eq!(
        certificate_acceptable(&certificate_with(restored.clone(), &scope, Vec::new())),
        Ok(()),
        "an out-of-scope mutation target is not a source when the certificate is spent"
    );
    assert_eq!(
        mutation_targets(&restored),
        Ok(vec![(
            "cargo test -p orbit-review --test fix guards_read_only",
            production.to_string()
        )]),
        "settlement compares the target at its repository-relative path"
    );
    let old_shape = mutation(&["crates/orbit-review/tests/fix.rs", production], &[]);
    let defect = validation_evidence(&old_shape, &context).expect_err("old shape is refused");
    assert_eq!(
        defect,
        ValidationDefect::ControlOutOfScope {
            command: "cargo test -p orbit-review --test fix guards_read_only".into(),
            source: production.into(),
        }
    );
    assert!(defect.correctable(), "{}", defect.reason());
    assert!(
        defect.reason().contains("mutation_target"),
        "{}",
        defect.reason()
    );

    let checks_out_of_scope = mutation(&[UNRELATED_FIXTURE], &[production]);
    assert_eq!(
        validation_evidence(&checks_out_of_scope, &context),
        Err(ValidationDefect::ControlOutOfScope {
            command: "cargo test -p orbit-review --test fix guards_read_only".into(),
            source: UNRELATED_FIXTURE.into(),
        }),
        "a mutation target never lets the control's checks leave the scope"
    );

    for observed in [
        ValidationDefect::RequiredNotPassed {
            command: CODEQL.into(),
            outcome: ValidationOutcome::Failed,
        },
        ValidationDefect::MutationTargetChanged {
            command: "cargo test".into(),
            target: production.into(),
        },
        ValidationDefect::DiagnosticInScope {
            command: WORKSPACE.into(),
            source: "crates/orbit-review/src/fix.rs".into(),
        },
    ] {
        assert!(
            !observed.correctable(),
            "a defect in what the checks observed is never returned for correction: {observed:?}"
        );
    }
}

/// [ORB-14632] Settlement compares each mutation target against the
/// repository, so a target must name a path a candidate's tree can hold.
/// Spellings of a repository-relative path read as that path; an absolute,
/// home-relative or out-of-repository path, Git's own store, and a selector
/// naming no single file are refused as a shape defect naming the target,
/// never passed for want of a match.
#[test]
fn a_mutation_target_must_be_a_repository_relative_path() {
    const COMMAND: &str = "cargo test -p orbit-review --test fix guards_read_only";
    let targets = |target: &str| {
        let mut record = control(
            COMMAND,
            Some(NegativeControl::Counterfactual),
            ValidationOutcome::Failed,
            &["crates/orbit-review/tests/fix.rs"],
        );
        record.mutation_target = vec![target.to_string(), " ".to_string()];
        mutation_targets(std::slice::from_ref(&record)).map(|targets| {
            targets
                .into_iter()
                .map(|(_, path)| path)
                .collect::<Vec<_>>()
        })
    };

    for spelled in [
        "crates/orbit-engine/src/converge.rs",
        "./crates/orbit-engine/src/converge.rs",
        " file:crates/orbit-engine/src/converge.rs ",
        "crates//orbit-engine/./src/converge.rs",
    ] {
        assert_eq!(
            targets(spelled),
            Ok(vec!["crates/orbit-engine/src/converge.rs".to_string()]),
            "{spelled}"
        );
    }

    for refused in [
        "/home/reviewer/wt/crates/orbit-engine/src/converge.rs",
        "~/wt/crates/orbit-engine/src/converge.rs",
        "C:/wt/crates/orbit-engine/src/converge.rs",
        "crates\\orbit-engine\\src\\converge.rs",
        "../other/crates/orbit-engine/src/converge.rs",
        "crates/../../converge.rs",
        ".git/config",
        "dir:crates/orbit-engine",
        "symbol:crates/orbit-engine/src/converge.rs::converge",
        "./",
    ] {
        let defect = targets(refused).expect_err(refused);
        assert_eq!(
            defect,
            ValidationDefect::MutationTargetInvalid {
                command: COMMAND.into(),
                target: refused.into(),
            }
        );
        assert!(defect.correctable(), "{}", defect.reason());
        assert!(defect.reason().contains(refused), "{}", defect.reason());
    }
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
            retired: &[],
            required_validation_commands: Some(&[]),
            baseline_commands: &[],
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
            retired: &[],
            required_validation_commands: Some(&[]),
            baseline_commands: &[],
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
            retired: &[],
            required_validation_commands: Some(&[]),
            baseline_commands: &[],
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
        retired: &[],
        required_validation_commands: Some(&host_required),
        baseline_commands: &[],
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
            ValidationDefect::TrustedCheckDiagnostic {
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

/// The incident shape [ORB-14684]: the reviewer filed a failed
/// `make ci-test-affected` run, which the owner lists in
/// `review.baseline_commands`, as a `diagnostic` whose self-reported sources
/// all lie outside the candidate. The owner trusts that gate, so the
/// reviewer's sources cannot excuse it: it passes, or carries a baseline
/// claim settlement reproduced on the pinned base.
const AFFECTED: &str = "make ci-test-affected";

fn incident_records() -> Vec<ReviewValidation> {
    let mut records = scoped_passes();
    records.push(diagnostic(
        &format!("CI_TEST_BASE=8d9d8b98 {AFFECTED}"),
        ValidationOutcome::Failed,
        &[UNRELATED_FIXTURE],
    ));
    records
}

#[test]
fn a_failed_trusted_gate_filed_as_diagnostic_is_never_coverage() {
    let task_scope = scope();
    let baseline = vec![AFFECTED.to_string()];
    let context = ValidationContext {
        scope: &task_scope,
        obligations: &[],
        retired: &[],
        required_validation_commands: Some(&[]),
        baseline_commands: &baseline,
    };
    let defect = validation_evidence(&incident_records(), &context)
        .expect_err("a listed gate's failure is not the reviewer's to excuse");
    assert_eq!(
        defect,
        ValidationDefect::TrustedCheckDiagnostic {
            command: format!("CI_TEST_BASE=8d9d8b98 {AFFECTED}"),
        }
    );
    let reason = defect.reason();
    assert!(
        reason.starts_with("validation_incomplete:")
            && reason.contains(AFFECTED)
            && reason.contains("passing record or a baseline claim"),
        "the reason names the command and what a trusted gate needs: {reason}"
    );
    assert!(
        !defect.correctable(),
        "relabeling cannot fix it without a base rerun"
    );

    // A `check` identity naming the listed command is the same gate.
    let mut wrapped = scoped_passes();
    let mut record = diagnostic(
        "set -o pipefail; make ci-test-affected | tee log",
        ValidationOutcome::Failed,
        &[UNRELATED_FIXTURE],
    );
    record.check = Some(AFFECTED.into());
    wrapped.push(record);
    assert!(matches!(
        validation_evidence(&wrapped, &context),
        Err(ValidationDefect::TrustedCheckDiagnostic { .. })
    ));

    // The certificate's own snapshot applies the same rule to its consumer.
    let mut certificate = certificate(incident_records());
    certificate.baseline_commands = baseline.clone();
    assert_eq!(
        certificate_acceptable(&certificate),
        Err(ReviewInvalidation::ValidationIncomplete)
    );
    assert!(exclusion(&exact_delivery(), &certificate, &facts()).is_err());
}

/// The neighbouring outcomes the trusted-gate rule leaves as they were
/// [ORB-14684].
#[test]
fn the_trusted_gate_rule_leaves_unlisted_diagnostics_and_required_records_alone() {
    let task_scope = scope();
    let baseline = vec![AFFECTED.to_string()];
    let context = ValidationContext {
        scope: &task_scope,
        obligations: &[],
        retired: &[],
        required_validation_commands: Some(&[]),
        baseline_commands: &baseline,
    };

    // An ad hoc observation the owner does not list keeps ORB-14192's
    // semantics: its out-of-scope failure is an honest diagnostic.
    let mut unlisted = scoped_passes();
    unlisted.push(diagnostic(
        WORKSPACE,
        ValidationOutcome::Failed,
        &[UNRELATED_FIXTURE],
    ));
    assert_eq!(validation_evidence(&unlisted, &context), Ok(()));
    let mut certificate_unlisted = certificate(unlisted);
    certificate_unlisted.baseline_commands = baseline.clone();
    assert_eq!(certificate_acceptable(&certificate_unlisted), Ok(()));

    // A failed required record of the listed gate with no baseline claim
    // still blocks as a required failure.
    let mut failed = scoped_passes();
    failed.push(required(AFFECTED, None, false));
    assert_eq!(
        validation_evidence(&failed, &context),
        Err(ValidationDefect::RequiredNotPassed {
            command: AFFECTED.into(),
            outcome: ValidationOutcome::Failed,
        })
    );

    // A passing record of the listed gate is unaffected, as `required` or
    // as a passing diagnostic.
    let mut passed = scoped_passes();
    passed.push(required(AFFECTED, None, true));
    assert_eq!(validation_evidence(&passed, &context), Ok(()));
    let mut observed = scoped_passes();
    observed.push(diagnostic(AFFECTED, ValidationOutcome::Passed, &[]));
    assert_eq!(validation_evidence(&observed, &context), Ok(()));
    let mut certificate_passed = certificate(passed);
    certificate_passed.baseline_commands = baseline;
    assert_eq!(certificate_acceptable(&certificate_passed), Ok(()));
}

/// A certificate issued before the snapshot carries no `baseline_commands`
/// [ORB-14684]. It stays readable, and the missing field means "no trusted
/// baseline commands": it never turns a refused record set into coverage,
/// and a missing required-check list is still no contract at all.
#[test]
fn a_certificate_without_the_baseline_snapshot_stays_readable_and_gains_no_pass() {
    let legacy = |certificate: &ReviewCertificate| -> ReviewCertificate {
        let mut value = serde_json::to_value(certificate).unwrap();
        let fields = value.as_object_mut().unwrap();
        assert!(
            !fields.contains_key("baseline_commands"),
            "an empty snapshot is not written, as before the field existed"
        );
        fields.remove("baseline_commands");
        serde_json::from_value(value).expect("a pre-snapshot certificate is readable")
    };

    let read = legacy(&certificate(incident_records()));
    assert!(read.baseline_commands.is_empty());
    assert_eq!(
        certificate_acceptable(&read),
        Ok(()),
        "judged exactly as when it was issued"
    );

    // A failed diagnostic of a captured required command is refused with
    // or without the baseline snapshot.
    let mut host_diagnostic = scoped_passes();
    host_diagnostic.push(required(AFFECTED, None, true));
    host_diagnostic.push(diagnostic(
        "make ci-fast",
        ValidationOutcome::Failed,
        &[UNRELATED_FIXTURE],
    ));
    let mut issued = certificate(host_diagnostic);
    issued.required_validation_commands = Some(vec!["make ci-fast".into()]);
    assert_eq!(
        certificate_acceptable(&legacy(&issued)),
        Err(ReviewInvalidation::ValidationIncomplete)
    );

    let mut contractless = certificate(incident_records());
    contractless.required_validation_commands = None;
    assert_eq!(
        certificate_acceptable(&legacy(&contractless)),
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
        retired: &[],
        required_validation_commands: Some(&host_required),
        baseline_commands: &[],
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
        retired: &[],
        required_validation_commands: Some(&required_commands),
        baseline_commands: &[],
    };
    let mut records = vec![
        record(
            command,
            Some("host-ci"),
            ValidationOutcome::Failed,
            ValidationRole::Superseded,
            Some("the runner environment was corrected"),
        ),
        required("make ci-fast --locked", Some("host-ci"), true),
    ];
    // Only the superseded record names the host command; the replacing pass
    // establishes it through their shared identity, in either report order.
    for order in ["attempt first", "pass first"] {
        assert_eq!(validation_evidence(&records, &context), Ok(()), "{order}");
        let mut certificate = certificate_with(records.clone(), &task_scope, Vec::new());
        certificate.required_validation_commands = Some(required_commands.clone());
        assert_eq!(certificate_acceptable(&certificate), Ok(()), "{order}");
        records.reverse();
    }
}

#[test]
fn missing_host_snapshot_is_not_an_empty_requirement_list() {
    let task_scope = scope();
    let context = ValidationContext {
        scope: &task_scope,
        obligations: &[],
        retired: &[],
        required_validation_commands: None,
        baseline_commands: &[],
    };
    assert_eq!(
        validation_evidence(&scoped_passes(), &context),
        Err(ValidationDefect::HostContractMissing)
    );
}
