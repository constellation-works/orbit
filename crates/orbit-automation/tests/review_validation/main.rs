#![allow(missing_docs, clippy::expect_used, clippy::unwrap_used)]

//! The review validation gate through the public review-coverage boundary.
//!
//! A reviewer may file a failed attempt as `superseded`. The attempt then
//! counts only when a required check that passed replaced it, in either report
//! order, with the same effective identity: a non-empty `check`, otherwise the
//! normalized command. Both consumers of these rules are driven here.
//! [`validation_evidence`] gives the reason the issuing gate escalates with.
//! [`certificate_acceptable`] and [`exclusion`] decide whether an issued
//! certificate covers a delivery. A certificate whose `validation_complete`
//! flag is set but whose records do not establish the replacement must never
//! be spent as coverage.

use std::sync::LazyLock;

use NegativeControl::{Counterfactual, PreFix, SupersededAssertion};
use ReviewInvalidation::{ValidationContractMissing, ValidationIncomplete};
use ValidationOutcome::{Denied, Failed, NotRun, Passed};
use ValidationRole::{Diagnostic, Excluded, ExpectedFailure, Required, Superseded};
use chrono::{TimeZone, Utc};
use orbit_automation::review::{
    LandingFacts, MutationTarget, SkippedCheck, ValidationContext, ValidationDefect,
    certificate_acceptable, exclusion, mutation_targets, validation_evidence,
    validation_limitations,
};
use orbit_types::workflow::automation::{Delivery, SourceRevision};
use orbit_types::workflow::{
    BaselineRedFailures, HostKnownRed, KnownRedValidation, NegativeControl,
    REVIEW_BASELINE_ARTIFACT, REVIEW_CONTRACT_VERSION, RecordGap, RetainedObligation,
    RetiredValidation, ReviewBaselineClaim, ReviewBudget, ReviewCertificate, ReviewConsumption,
    ReviewInvalidation, ReviewValidation, ReviewVerdict, ReviewerIdentity, ValidationOutcome,
    ValidationRole,
};

const ATTEMPT: &str = "cargo test --package orbit-core";
/// The workspace-wide diagnostic ORB-14151's reviewer ran on its final
/// candidate; it failed only in engine fixtures the task never touched.
const WORKSPACE: &str = "cargo test --workspace --no-fail-fast";
const UNRELATED_FIXTURE: &str = "crates/orbit-engine/tests/fixtures/f066_resume.rs";
/// ORB-14191's required CodeQL run, which its replacement report omitted.
const CODEQL: &str = "scripts/codeql-rust-local.sh --ram 16384 \
     codeql/rust-queries:codeql-suites/rust-security-extended.qls";
const FAST: &str = "make ci-fast";
const LINT: &str = "make ci-lint";
const IN_SCOPE: &str = "crates/orbit-review/src/fix.rs";
const SCOPED_TEST: &str = "crates/orbit-review/tests/fix.rs";

/// The candidate's scope: its task selectors and every path it changed.
static SCOPE: LazyLock<Vec<String>> = LazyLock::new(|| {
    vec![
        "dir:crates/orbit-review".to_string(),
        format!("file:{IN_SCOPE}"),
        format!("file:{SCOPED_TEST}"),
    ]
});
/// Differs from [`ATTEMPT`] by an argument. A leading environment assignment
/// is the same command, not this fixture [ORB-14302].
const CORRECTED: &str = "cargo test --package orbit-core --locked";

#[test]
fn replacement_relationships_that_are_missing_ambiguous_invalid_or_not_passing_fail_closed() {
    let core = Some("orbit-core-tests");
    let attempt = |check: Option<&str>| superseded(ATTEMPT, check);
    let pass = |command: &str, check: Option<&str>| required(command, check, true);
    let fmt = "cargo fmt --check";
    let types = Some("cargo test --package orbit-types");
    let (units, formatting) = (Some("unit-tests"), Some("formatting"));
    let cases = [
        (
            "missing: a corrected command with no shared check identity",
            vec![attempt(None), pass(CORRECTED, None)],
        ),
        (
            "missing: no same-identity required pass anywhere",
            vec![pass(LINT, None), attempt(None), pass(fmt, None)],
        ),
        (
            "invalid: a broader check identity with the same command",
            vec![
                pass(ATTEMPT, Some("runtime tests and formatting")),
                attempt(Some("runtime tests")),
                pass(ATTEMPT, Some("runtime tests and formatting")),
            ],
        ),
        (
            "ambiguous: an identity on the attempt only",
            vec![attempt(core), pass(CORRECTED, None)],
        ),
        (
            "ORB-14745: the attempt's explicit identity overrides an identical command",
            vec![attempt(core), pass(ATTEMPT, None)],
        ),
        (
            "ORB-14745: the replacement's explicit identity overrides an identical command",
            vec![attempt(None), pass(ATTEMPT, core)],
        ),
        (
            "missing: an explicit identity names a different command",
            vec![attempt(None), pass(CORRECTED, types)],
        ),
        (
            "invalid: an empty check identity relates no corrected command",
            vec![attempt(Some("")), pass(CORRECTED, Some(""))],
        ),
        (
            "invalid: a whitespace check identity relates no corrected command",
            vec![attempt(Some("   ")), pass(CORRECTED, Some("   "))],
        ),
        (
            "invalid: identities naming different checks",
            vec![attempt(units), pass(CORRECTED, formatting)],
        ),
        (
            "non-passing: the related required check failed",
            vec![attempt(core), required(CORRECTED, core, false)],
        ),
        (
            "non-passing: the related replacement is not a pass",
            vec![
                attempt(core),
                record(CORRECTED, core, NotRun, Required, None),
                pass(FAST, None),
            ],
        ),
        (
            "non-passing: the related pass is not a required check",
            vec![
                attempt(core),
                record(CORRECTED, core, Passed, Superseded, Some("also superseded")),
                pass(FAST, None),
            ],
        ),
    ];
    for (name, records) in cases {
        let defect = default_evidence(&records)
            .expect_err("an unreplaced superseded attempt must not validate the candidate");
        assert_eq!(defect, unreplaced(ATTEMPT), "{name}");
        let reason = defect.reason();
        assert!(
            reason.starts_with("validation_incomplete:"),
            "{name}: the gate escalates as incomplete validation: {reason}"
        );
        assert_uncovered(&certificate(records), name);
    }
}

/// The accepting shapes the refusals above are measured against: the same
/// effective identity, even when only one record explicitly supplies it
/// [ORB-13894: a final `make ci-fast` pass without `check` settled a passing
/// review as incomplete].
#[test]
fn a_superseded_attempt_replaced_by_the_same_check_is_coverage() {
    let attempt = |check: Option<&str>| superseded(ATTEMPT, check);
    let pass = |command: &str, check: Option<&str>| required(command, check, true);
    let spaced = format!("  {}", ATTEMPT.replace(' ', "  "));
    let prefixed = format!("ORBIT_TEST_ALLOWLIST=1 {ATTEMPT}");
    let (core, padded) = (Some("orbit-core-tests"), Some(" orbit-core-tests "));
    // [ORB-19519] Two rustdoc attempts whose explicit `check` keeps the
    // `RUSTDOCFLAGS` assignment are replaced by the required passes of the
    // same commands, which derive their identity without it. Shaped like
    // jrun-20261028-1120-c1, whose settlement stayed `incomplete` on this pair.
    let mut rustdoc = Vec::new();
    for package in ["orbit-core", "orbit-exec"] {
        let command = format!("cargo doc --no-deps -p {package}");
        let flagged = format!("RUSTDOCFLAGS=\"-D warnings\" {command}");
        rustdoc.extend([superseded(&flagged, Some(&flagged)), pass(&command, None)]);
    }
    for (name, mut records) in [
        (
            "same command rerun",
            vec![attempt(None), pass(ATTEMPT, None)],
        ),
        (
            "replacement command matches the attempt's explicit identity",
            vec![attempt(Some(ATTEMPT)), pass(ATTEMPT, None)],
        ),
        (
            "replacement identity matches the attempt's command",
            vec![attempt(None), pass(CORRECTED, Some(ATTEMPT))],
        ),
        (
            "same command rerun with an empty check and different spacing",
            vec![attempt(Some("")), pass(&spaced, None)],
        ),
        (
            "corrected command with a shared check identity",
            vec![attempt(core), pass(CORRECTED, padded)],
        ),
        (
            "leading environment assignment is the same command",
            vec![attempt(None), pass(&prefixed, None)],
        ),
        ("ORB-19519: env-prefixed rustdoc identities", rustdoc),
    ] {
        // ORB-14322: final-candidate required passes replace the same check
        // regardless of report order, at issuance and coverage consumption.
        for order in ["attempt first", "pass first"] {
            assert_eq!(default_evidence(&records), Ok(()), "{name}, {order}");
            let issued = certificate(records.clone());
            assert_eq!(certificate_acceptable(&issued), Ok(()), "{name}, {order}");
            let tree = exclusion(&exact_delivery(), &issued, &facts())
                .unwrap_or_else(|reason| panic!("{name}, {order}: {reason:?}"))
                .final_candidate_tree;
            assert_eq!(tree, "tree-final", "{name}, {order}");
            records.reverse();
        }
    }
}

/// [ORB-19519] Normalizing the explicit identity strips only the assignment:
/// the same env-prefixed check for another command still leaves the attempt
/// unreplaced.
#[test]
fn an_env_prefixed_explicit_identity_naming_another_command_still_refuses() {
    let attempt = "RUSTDOCFLAGS=\"-D warnings\" cargo doc --no-deps -p orbit-core";
    let records = vec![
        superseded(attempt, Some(attempt)),
        required("cargo doc --no-deps -p orbit-exec", None, true),
    ];
    assert_eq!(default_evidence(&records), Err(unreplaced(attempt)));
}

/// Without any replacement question, a required check that did not pass and
/// a set with no required pass are refused at both boundaries too.
#[test]
fn a_set_without_a_passing_required_check_is_never_coverage() {
    let note = "the superseded assertion must no longer hold";
    let control = expected_failure("grep -q legacy-header old.conf", note);
    for (name, records) in [
        ("empty", Vec::new()),
        ("required check failed", vec![required(FAST, None, false)]),
        ("only an explained negative control", vec![control]),
    ] {
        assert!(default_evidence(&records).is_err(), "{name}");
        assert_eq!(coverage(records), Err(ValidationIncomplete), "{name}");
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
        repaired_blob: Default::default(),
        deferred: Vec::new(),
        baseline: None,
    }
}

fn superseded(command: &str, check: Option<&str>) -> ReviewValidation {
    let note = Some("sandbox allowlist leak; rerun after the fix");
    record(command, check, Failed, Superseded, note)
}

fn required(command: &str, check: Option<&str>, passed: bool) -> ReviewValidation {
    let outcome = if passed { Passed } else { Failed };
    record(command, check, outcome, Required, None)
}

/// A required check that never ran.
fn not_run(command: &str) -> ReviewValidation {
    record(command, None, NotRun, Required, None)
}

/// A check that never ran, excluded with `note`.
fn excluded(command: &str, note: &str) -> ReviewValidation {
    record(command, None, NotRun, Excluded, Some(note))
}

/// A failure filed as an expected failure with only a note, no control.
fn expected_failure(command: &str, note: &str) -> ReviewValidation {
    record(command, None, Failed, ExpectedFailure, Some(note))
}

fn diagnostic(command: &str, outcome: ValidationOutcome, sources: &[&str]) -> ReviewValidation {
    let note = Some("workspace-wide observation outside the task's scope");
    let mut record = record(command, None, outcome, Diagnostic, note);
    record.sources = sources.iter().map(|source| (*source).to_string()).collect();
    record
}

fn control(
    command: &str,
    kind: NegativeControl,
    outcome: ValidationOutcome,
    sources: &[&str],
) -> ReviewValidation {
    let note = Some("the regression the fix removes");
    let mut record = record(command, None, outcome, ExpectedFailure, note);
    record.control = Some(kind);
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

fn with_id(mut record: ReviewValidation, id: &str) -> ReviewValidation {
    record.id = Some(id.into());
    record
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
    certificate_with(validation, &SCOPE, Vec::new())
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
        host_overrides: Vec::new(),
        known_red: Vec::new(),
        host_evidence: Vec::new(),
        issued_at: Utc.with_ymd_and_hms(2026, 10, 3, 0, 0, 0).unwrap(),
        owed_evidence: Vec::new(),
        owner_owed_evidence: Vec::new(),
        host_validation: Vec::new(),
        resumed_hold_attempt: None,
    }
}

/// [`certificate`] issued with the host's captured required commands.
fn certificate_requiring(records: Vec<ReviewValidation>, commands: &[String]) -> ReviewCertificate {
    let mut certificate = certificate(records);
    certificate.required_validation_commands = Some(commands.to_vec());
    certificate
}

/// [`certificate`] issued with the owner's `review.baseline_commands`.
fn certificate_trusting(records: Vec<ReviewValidation>, baseline: &[String]) -> ReviewCertificate {
    let mut certificate = certificate(records);
    certificate.baseline_commands = baseline.to_vec();
    certificate
}

/// Whether [`certificate`] over `validation` is acceptable coverage.
fn coverage(validation: Vec<ReviewValidation>) -> Result<(), ReviewInvalidation> {
    certificate_acceptable(&certificate(validation))
}

/// A certificate whose flag claims complete validation is spent neither on
/// its own nor for an exact-tree delivery.
fn assert_uncovered(certificate: &ReviewCertificate, name: &str) {
    assert_eq!(
        certificate_acceptable(certificate),
        Err(ValidationIncomplete),
        "{name}: the flag alone is not coverage"
    );
    let delivered = exclusion(&exact_delivery(), certificate, &facts()).map(|_| ());
    assert_eq!(
        delivered,
        Err(ValidationIncomplete),
        "{name}: an exact-tree delivery stays uncovered"
    );
}

/// The certificate's consumer agrees with issuance: it is coverage exactly
/// when issuance established the records.
fn assert_consumer_agrees(
    certificate: &ReviewCertificate,
    issued: &Result<(), ValidationDefect>,
    name: &str,
) {
    let expected = issued.is_ok().then_some(()).ok_or(ValidationIncomplete);
    let observed = certificate_acceptable(certificate);
    assert_eq!(observed, expected, "{name}: certificate coverage");
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
        landed_head: None,
        rebased_tree: None,
    }
}

/// A context judged against [`SCOPE`] with every other field defaulted.
fn scoped() -> ValidationContext<'static> {
    ValidationContext {
        scope: &SCOPE,
        ..ValidationContext::default()
    }
}

fn default_evidence(records: &[ReviewValidation]) -> Result<(), ValidationDefect> {
    validation_evidence(records, &ValidationContext::default())
}

fn scoped_evidence(records: &[ReviewValidation]) -> Result<(), ValidationDefect> {
    validation_evidence(records, &scoped())
}

/// The task's nine scoped regressions, all passing on the final candidate.
fn scoped_passes() -> Vec<ReviewValidation> {
    let command = |index| format!("cargo test -p orbit-review --test fix case_{index}");
    (1..=9)
        .map(|index| required(&command(index), None, true))
        .collect()
}

/// [`scoped_passes`] followed by `record`.
fn scoped_passes_and(record: ReviewValidation) -> Vec<ReviewValidation> {
    let mut records = scoped_passes();
    records.push(record);
    records
}

fn unreplaced(command: &str) -> ValidationDefect {
    let command = command.into();
    ValidationDefect::SupersededWithoutReplacement { command }
}

fn not_passed(command: &str, outcome: ValidationOutcome) -> ValidationDefect {
    let command = command.into();
    ValidationDefect::RequiredNotPassed { command, outcome }
}

fn host_unestablished(command: &str) -> ValidationDefect {
    let command = command.into();
    ValidationDefect::HostCheckNotEstablished { command }
}

fn unevidenced(command: &str, role: ValidationRole, missing: &'static str) -> ValidationDefect {
    ValidationDefect::ClassificationUnevidenced {
        command: command.into(),
        role,
        missing,
    }
}

fn out_of_scope(command: &str, source: &str) -> ValidationDefect {
    let (command, source) = (command.into(), source.into());
    ValidationDefect::ControlOutOfScope { command, source }
}

fn in_scope(command: &str, source: &str) -> ValidationDefect {
    let (command, source) = (command.into(), source.into());
    ValidationDefect::DiagnosticInScope { command, source }
}

fn contradicted(
    command: &str,
    role: ValidationRole,
    outcome: ValidationOutcome,
) -> ValidationDefect {
    ValidationDefect::RoleContradicted {
        command: command.into(),
        role,
        outcome,
    }
}

fn check_contradicted(command: &str, role: ValidationRole) -> ValidationDefect {
    let command = command.into();
    ValidationDefect::CheckContradicted { command, role }
}

fn dropped(
    command: &str,
    outcome: ValidationOutcome,
    role: Option<ValidationRole>,
) -> ValidationDefect {
    ValidationDefect::ObligationDropped {
        command: command.into(),
        outcome,
        role,
    }
}

fn trusted_diagnostic(command: &str) -> ValidationDefect {
    let command = command.into();
    ValidationDefect::TrustedCheckDiagnostic { command }
}

/// Retained obligations match by effective identity across report revisions
/// [ORB-14312], while different commands and explicit identities stay distinct.
#[test]
fn retained_obligations_match_effective_identities_across_report_revisions() {
    const WRAPPED: &str = "set -o pipefail; make ci-fast 2>&1 | tee .orbit/tmp/review-ci-fast.log";
    const PREFIXED: &str = "TMPDIR=\"$PWD/.orbit/tmp\" FOO='two words' make  ci-fast";
    let trimmed = Some(" make ci-fast ");
    let matching = [
        (
            "command to wrapped explicit identity",
            [(FAST, None), (WRAPPED, Some(FAST))],
        ),
        (
            "wrapped explicit identity to command",
            [(WRAPPED, Some(FAST)), (FAST, None)],
        ),
        (
            "normalized command to trimmed explicit identity",
            [(PREFIXED, None), (WRAPPED, trimmed)],
        ),
        (
            "trimmed explicit identity to normalized command",
            [(WRAPPED, trimmed), (PREFIXED, None)],
        ),
    ];
    let distinct = [
        ("different bare commands", [(FAST, None), (LINT, None)]),
        (
            "different command suffix",
            [(FAST, None), ("make ci-fast-extra", None)],
        ),
        (
            "command differs from final explicit identity",
            [(FAST, None), (WRAPPED, Some(LINT))],
        ),
        (
            "earlier explicit identity differs from command",
            [(WRAPPED, Some(LINT)), (FAST, None)],
        ),
        (
            "distinct explicit identities override identical commands",
            [(FAST, Some("fast-check")), (FAST, Some("lint-check"))],
        ),
        (
            "explicit identity overrides the earlier command",
            [(FAST, Some("fast-check")), (FAST, None)],
        ),
    ];
    let cases =
        (matching.map(|case| (case, true)).into_iter()).chain(distinct.map(|case| (case, false)));
    for ((name, [(earlier, earlier_check), (current, current_check)]), matches) in cases {
        let earlier_record = record(earlier, earlier_check, NotRun, Required, None);
        let obligations = vec![retained(earlier_record)];
        let records = vec![required(current, current_check, true)];
        let context = ValidationContext {
            obligations: &obligations,
            ..ValidationContext::default()
        };
        let expected = matches
            .then_some(())
            .ok_or_else(|| dropped(earlier, NotRun, None));
        let observed = validation_evidence(&records, &context);
        assert_eq!(observed, expected, "ORB-14312: {name}");
        let certificate = certificate_with(records, &SCOPE, obligations);
        assert_consumer_agrees(&certificate, &expected, &format!("ORB-14312: {name}"));
        let delivered = exclusion(&exact_delivery(), &certificate, &facts()).map(|_| ());
        let acceptable = certificate_acceptable(&certificate);
        assert_eq!(
            delivered, acceptable,
            "ORB-14312: {name}: delivery coverage"
        );
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
    let unrun = |command: &str| with_id(not_run(command), "V1");
    let failed = |command: &str| with_id(required(command, None, false), "V1");
    let passed = |command: &str, id: &str| with_id(required(command, None, true), id);
    let rerun = with_id(superseded(FAST, None), "V1");
    let tee = passed("set -o pipefail; make ci-fast | tee log", "V1");
    let diagnosed = with_id(diagnostic(SCRIPT, Passed, &[]), "V1");
    // Each case pairs the earlier record with the final report, retires `V1`
    // with the given reason, if any, and expects the given gap, if any, to
    // drop the earlier record.
    let cases = [
        (
            "ORB-14360: prose name to the real command",
            (unrun(PROSE), vec![passed(SCRIPT, "V1")]),
            (None, None),
        ),
        (
            "ORB-14260: placeholder to the absolute path",
            (unrun(PLACEHOLDER), vec![passed(ABSOLUTE, "V1")]),
            (None, None),
        ),
        (
            "a failed record rerun under its id with a new command",
            (failed(FAST), vec![rerun, tee]),
            (None, None),
        ),
        (
            "an unrun record retired with a reason",
            (unrun(PROSE), vec![passed(SCRIPT, "V2")]),
            (Some("folded into V2"), None),
        ),
        (
            "the same command under another id",
            (unrun(SCRIPT), vec![passed(SCRIPT, "V2")]),
            (None, Some(RecordGap::Omitted)),
        ),
        (
            "the id carried only as a diagnostic",
            (unrun(PROSE), vec![passed(LINT, "V2"), diagnosed]),
            (None, Some(RecordGap::Reclassified(Diagnostic))),
        ),
        (
            "a retirement without a reason",
            (unrun(PROSE), vec![passed(SCRIPT, "V2")]),
            (Some("  "), Some(RecordGap::RetirementUnexplained)),
        ),
        (
            "a failed record retired",
            (failed(FAST), vec![passed(LINT, "V2")]),
            (Some("flaky"), Some(RecordGap::FailureRetired)),
        ),
    ];
    let retire = |reason: &str| RetiredValidation {
        id: "V1".into(),
        reason: reason.into(),
    };
    for (name, (earlier, records), (retirement, gap)) in cases {
        let retired: Vec<_> = retirement.map(retire).into_iter().collect();
        let expected = gap.map_or(Ok(()), |gap| {
            Err(ValidationDefect::RecordDropped {
                id: "V1".into(),
                command: earlier.command.clone(),
                outcome: earlier.outcome,
                gap,
            })
        });
        let obligations = vec![retained(earlier)];
        let context = ValidationContext {
            obligations: &obligations,
            retired: &retired,
            ..ValidationContext::default()
        };
        assert_eq!(validation_evidence(&records, &context), expected, "{name}");
        let mut certificate = certificate_with(records, &SCOPE, obligations);
        certificate.retired_validation = retired;
        assert_consumer_agrees(&certificate, &expected, name);
    }
}

/// A certificate issued before record ids, stored without `id` or
/// `retired_validation`, is read back and spent under the command-identity
/// rules it was issued under.
#[test]
fn certificates_without_record_ids_keep_their_coverage() {
    let obligations = vec![retained(not_run(FAST))];
    for (current, covered) in [
        ("TMPDIR=\"$PWD/.orbit/tmp\" make ci-fast", true),
        (LINT, false),
    ] {
        let records = vec![required(current, None, true)];
        let issued = certificate_with(records, &SCOPE, obligations.clone());
        let stored = serde_json::to_value(&issued).unwrap();
        assert!(stored.get("retired_validation").is_none());
        assert!(stored["validation"][0].get("id").is_none());
        let certificate: ReviewCertificate = serde_json::from_value(stored).unwrap();
        let acceptable = certificate_acceptable(&certificate).is_ok();
        assert_eq!(acceptable, covered, "{current}");
    }
}

/// ORB-14151's shape: every task-scoped regression passed and a final-candidate
/// workspace run failed only in unrelated fixtures. Recorded as diagnostics, the
/// failures keep their exact outcome and sources, the bounded task's validation
/// is complete, and the certificate says what it did not establish instead of
/// claiming the workspace passed.
#[test]
fn an_unrelated_workspace_failure_is_an_honest_diagnostic_not_a_blocker_or_a_control() {
    const CLAIM_FIXTURE: &str = "crates/orbit-engine/tests/fixtures/f071_claim.rs";
    const ENGINE: &str = "cargo test -p orbit-engine f066";
    let mut records = scoped_passes();
    records.extend([
        diagnostic(WORKSPACE, Failed, &[UNRELATED_FIXTURE, CLAIM_FIXTURE]),
        diagnostic(ENGINE, Failed, &[UNRELATED_FIXTURE]),
    ]);
    assert_eq!(scoped_evidence(&records), Ok(()));
    let certificate = certificate(records.clone());
    assert_eq!(certificate_acceptable(&certificate), Ok(()));
    exclusion(&exact_delivery(), &certificate, &facts())
        .expect("the bounded task is covered without a workspace-green claim");
    assert_eq!(
        validation_limitations(&certificate.validation),
        vec![
            format!("diagnostic `{WORKSPACE}` failed in {UNRELATED_FIXTURE}, {CLAIM_FIXTURE}"),
            format!("diagnostic `{ENGINE}` failed in {UNRELATED_FIXTURE}"),
        ],
        "the certificate discloses exactly what it does not establish"
    );
    let mut diagnostics = certificate
        .validation
        .iter()
        .filter(|record| record.role == Diagnostic);
    assert!(
        diagnostics.all(|record| record.outcome == Failed),
        "raw failed outcomes are kept, never rewritten as passes"
    );

    // The same observations filed as expected failures, as ORB-14151's
    // report did, are not deliberate controls and never pass.
    let mislabeled = scoped_passes_and(expected_failure(WORKSPACE, "out of scope for this task"));
    let defect = unevidenced(WORKSPACE, ExpectedFailure, "control");
    assert_eq!(scoped_evidence(&mislabeled), Err(defect));
    assert_eq!(coverage(mislabeled), Err(ValidationIncomplete));
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
    const COMMAND: &str = "cargo test -p orbit-review --test fix guards_read_only";
    const PRODUCTION: &str = "crates/orbit-engine/src/converge.rs";
    let mutation = |sources: &[&str], targets: &[&str]| {
        let mut record = control(COMMAND, Counterfactual, Failed, sources);
        record.mutation_target = targets.iter().map(|target| (*target).to_string()).collect();
        scoped_passes_and(record)
    };

    let restored = mutation(&[SCOPED_TEST], &[PRODUCTION]);
    assert_eq!(scoped_evidence(&restored), Ok(()));
    assert_eq!(
        coverage(restored.clone()),
        Ok(()),
        "an out-of-scope mutation target is not a source when the certificate is spent"
    );
    let target = MutationTarget {
        command: COMMAND,
        path: PRODUCTION.to_string(),
        repaired_blob: None,
    };
    assert_eq!(
        mutation_targets(&restored),
        Ok(vec![target]),
        "settlement compares the target at its repository-relative path"
    );
    let old_shape = mutation(&[SCOPED_TEST, PRODUCTION], &[]);
    let defect = scoped_evidence(&old_shape).expect_err("old shape is refused");
    assert_eq!(defect, out_of_scope(COMMAND, PRODUCTION));
    let reason = defect.reason();
    assert!(defect.correctable(), "{reason}");
    assert!(reason.contains("mutation_target"), "{reason}");

    let checks_out_of_scope = mutation(&[UNRELATED_FIXTURE], &[PRODUCTION]);
    assert_eq!(
        scoped_evidence(&checks_out_of_scope),
        Err(out_of_scope(COMMAND, UNRELATED_FIXTURE)),
        "a mutation target never lets the control's checks leave the scope"
    );

    let (command, target) = ("cargo test".into(), PRODUCTION.into());
    let changed = ValidationDefect::MutationTargetChanged { command, target };
    for observed in [
        not_passed(CODEQL, Failed),
        changed,
        in_scope(WORKSPACE, IN_SCOPE),
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
        let mut record = control(COMMAND, Counterfactual, Failed, &[SCOPED_TEST]);
        record.mutation_target = vec![target.to_string(), " ".to_string()];
        let targets = mutation_targets(std::slice::from_ref(&record))?;
        let paths = targets.into_iter().map(|target| target.path);
        Ok::<_, ValidationDefect>(paths.collect::<Vec<_>>())
    };

    let canonical = vec!["crates/orbit-engine/src/converge.rs".to_string()];
    for spelled in [
        "crates/orbit-engine/src/converge.rs",
        "./crates/orbit-engine/src/converge.rs",
        " file:crates/orbit-engine/src/converge.rs ",
        "crates//orbit-engine/./src/converge.rs",
    ] {
        assert_eq!(targets(spelled), Ok(canonical.clone()), "{spelled}");
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
        let (command, target) = (COMMAND.into(), refused.into());
        assert_eq!(
            defect,
            ValidationDefect::MutationTargetInvalid { command, target }
        );
        let reason = defect.reason();
        assert!(defect.correctable(), "{reason}");
        assert!(reason.contains(refused), "{reason}");
    }
}

/// A required check that failed, was denied, never ran, or was dropped cannot
/// be cleared by changing its role or note, beside unrelated passing checks,
/// at issuance or when a certificate is spent.
#[test]
fn relabeling_or_dropping_a_required_check_never_completes_validation() {
    let codeql_failed = || vec![retained(required(CODEQL, None, false))];
    let unrelated = |command: &str| diagnostic(command, Failed, &[UNRELATED_FIXTURE]);
    let relabeled = [
        (
            "a failed check relabeled expected_failure with only a note",
            vec![expected_failure(FAST, "known flake")],
            unevidenced(FAST, ExpectedFailure, "control"),
        ),
        (
            "a control naming code outside the candidate's scope",
            vec![control(FAST, Counterfactual, Failed, &[UNRELATED_FIXTURE])],
            out_of_scope(FAST, UNRELATED_FIXTURE),
        ),
        (
            "a failed check relabeled diagnostic with only a note",
            vec![diagnostic(FAST, Failed, &[])],
            unevidenced(FAST, Diagnostic, "sources"),
        ),
        (
            "a diagnostic whose failure lies in the candidate's own code",
            vec![diagnostic(FAST, Failed, &[UNRELATED_FIXTURE, IN_SCOPE])],
            in_scope(FAST, IN_SCOPE),
        ),
        (
            "a diagnostic failing the same check a required record passed",
            vec![unrelated(WORKSPACE), required(WORKSPACE, None, true)],
            check_contradicted(WORKSPACE, Diagnostic),
        ),
        (
            "a denied check relabeled diagnostic",
            vec![diagnostic(CODEQL, Denied, &[])],
            contradicted(CODEQL, Diagnostic, Denied),
        ),
    ];
    // Each of these final reports follows a report that retained CodeQL's failure.
    let retained_failure = [
        (
            "a retained required failure the final report omits (ORB-14191)",
            Vec::new(),
            dropped(CODEQL, Failed, None),
        ),
        (
            "a retained required failure the final report calls a diagnostic",
            vec![unrelated(CODEQL)],
            dropped(CODEQL, Failed, Some(Diagnostic)),
        ),
        (
            "a retained required failure the final report calls a control",
            vec![control(CODEQL, PreFix, Failed, &[IN_SCOPE])],
            dropped(CODEQL, Failed, Some(ExpectedFailure)),
        ),
        (
            "a retained required failure the final report calls excluded",
            vec![excluded(CODEQL, "not needed")],
            dropped(CODEQL, Failed, Some(Excluded)),
        ),
    ];
    let relabeled =
        relabeled.map(|(name, records, expected)| (name, records, Vec::new(), expected));
    let retained_failure = retained_failure
        .map(|(name, records, expected)| (name, records, codeql_failed(), expected));
    let cases = relabeled.into_iter().chain(retained_failure);
    for (name, mut records, obligations, expected) in cases {
        // Unrelated passing checks never pay for the defect.
        records.extend(scoped_passes());
        let context = ValidationContext {
            obligations: &obligations,
            ..scoped()
        };
        let observed = validation_evidence(&records, &context);
        assert_eq!(observed, Err(expected), "{name}");
        assert_uncovered(&certificate_with(records, &SCOPE, obligations), name);
    }

    // A failed diagnostic judged with no recorded scope proves nothing about
    // where its failures lie.
    let unscoped = scoped_passes_and(unrelated(WORKSPACE));
    let command = WORKSPACE.into();
    let scope_unknown = ValidationDefect::ScopeUnknown { command };
    assert_eq!(default_evidence(&unscoped), Err(scope_unknown));
    let unscoped_certificate = certificate_with(unscoped, &[], Vec::new());
    let observed = certificate_acceptable(&unscoped_certificate);
    assert_eq!(observed, Err(ValidationIncomplete));
}

/// Deliberate controls, retained obligations a later record legitimately
/// resolves, and ORB-11528's exclusion and supersession stay coverage; a must-
/// fail control that passed, or one run on the candidate that a required pass
/// of the same check contradicts, does not.
#[test]
fn deliberate_controls_and_resolved_obligations_remain_coverage() {
    const TEST: &str = "cargo test -p orbit-review --test fix regression";
    const MUTATION: &str = "cargo test -p orbit-review --test fix rejects_mutation";
    const DEPLOY: &str = "orbit deploy production";
    let pass = || required(TEST, None, true);
    let retained_failure = || vec![retained(required(CODEQL, None, false))];
    let mutation = control(
        MUTATION,
        Counterfactual,
        Failed,
        &["dir:crates/orbit-review"],
    );
    let replacement = required("ORBIT_RAM=1 codeql", Some(CODEQL), true);
    let unauthorized = excluded(DEPLOY, "deployment is not authorized by this task");
    let accepted = [
        (
            "a pre-fix reproduction of the check that now passes",
            vec![control(TEST, PreFix, Failed, &[SCOPED_TEST]), pass()],
            Vec::new(),
        ),
        (
            "a counterfactual run on the candidate",
            vec![mutation, pass()],
            Vec::new(),
        ),
        (
            "a retained failure rerun and passed",
            vec![required(CODEQL, None, true)],
            retained_failure(),
        ),
        (
            "a retained failure superseded and replaced by the same check",
            vec![superseded(CODEQL, Some(CODEQL)), replacement],
            retained_failure(),
        ),
        (
            "a retained unperformed action later excluded as out of scope",
            vec![unauthorized, pass()],
            vec![retained(not_run(DEPLOY))],
        ),
        (
            "role-less legacy records read as required checks",
            vec![pass()],
            Vec::new(),
        ),
    ];
    for (name, records, obligations) in accepted {
        let context = ValidationContext {
            obligations: &obligations,
            ..scoped()
        };
        assert_eq!(validation_evidence(&records, &context), Ok(()), "{name}");
        let certificate = certificate_with(records, &SCOPE, obligations);
        assert_eq!(certificate_acceptable(&certificate), Ok(()), "{name}");
    }

    let candidate_side = control(TEST, SupersededAssertion, Failed, &[SCOPED_TEST]);
    let refused = [
        (
            "a must-fail control that passed",
            vec![control(TEST, PreFix, Passed, &[SCOPED_TEST]), pass()],
            contradicted(TEST, ExpectedFailure, Passed),
        ),
        (
            "a candidate-side control the same required pass contradicts",
            vec![candidate_side, pass()],
            check_contradicted(TEST, ExpectedFailure),
        ),
        (
            "a control naming no sources",
            vec![control(TEST, PreFix, Failed, &[]), pass()],
            unevidenced(TEST, ExpectedFailure, "sources"),
        ),
    ];
    for (name, records, expected) in refused {
        assert_eq!(scoped_evidence(&records), Err(expected), "{name}");
        assert_eq!(coverage(records), Err(ValidationIncomplete), "{name}");
    }
}

/// The host's captured required command is established only by a passing
/// required record of that check, at issuance and when the certificate is
/// spent. ORB-14302: reviewers prefix `TMPDIR` because a nested temp
/// directory is not hermetic. That required pass is the host command. A
/// different command is not, including one that only grows the name or
/// changes the program after its own leading assignment.
#[test]
fn captured_host_checks_cannot_be_omitted_or_reclassified() {
    let host_required = vec![FAST.to_string()];
    let context = ValidationContext {
        required_validation_commands: Some(&host_required),
        ..scoped()
    };
    let unestablished = || Err(host_unestablished(FAST));
    let env_wrapped = r#"env TMPDIR="$PWD/.orbit/tmp" make ci-fast"#;
    let denied = record(FAST, None, Denied, Required, None);
    let with = scoped_passes_and;
    let pass = |command| vec![required(command, None, true)];
    let mut cases = vec![
        ("missing", scoped_passes(), unestablished()),
        (
            "failed",
            with(required(FAST, None, false)),
            Err(not_passed(FAST, Failed)),
        ),
        ("denied", with(denied), Err(not_passed(FAST, Denied))),
        (
            "diagnostic substitution",
            scoped_passes_and(diagnostic(FAST, Failed, &[UNRELATED_FIXTURE])),
            Err(trusted_diagnostic(FAST)),
        ),
        (
            "negative-control substitution",
            scoped_passes_and(control(FAST, PreFix, Failed, &[IN_SCOPE])),
            unestablished(),
        ),
        (
            "excluded substitution",
            scoped_passes_and(excluded(FAST, "the check was not available")),
            unestablished(),
        ),
        ("passed", with(required(FAST, None, true)), Ok(())),
        (
            "a non-assignment wrap satisfies the host command when check names it",
            vec![required(env_wrapped, Some(FAST), true)],
            Ok(()),
        ),
        (
            "a non-assignment wrap without check is a different command",
            vec![required(env_wrapped, None, true)],
            unestablished(),
        ),
    ];
    for command in [
        r#"TMPDIR="$PWD/.orbit/tmp" make ci-fast"#,
        "TMPDIR='$PWD/.orbit/tmp' make ci-fast",
        "TMPDIR=$PWD/.orbit/tmp make ci-fast",
        r#"FOO=1 TMPDIR="$PWD/.orbit/tmp" make ci-fast"#,
        r#"TMPDIR="/tmp/orbit tmp" make  ci-fast"#,
    ] {
        cases.push((command, pass(command), Ok(())));
    }
    for command in [
        "make ci-fast-extra",
        "FOO=1 make other",
        r#"TMPDIR="$PWD/.orbit/tmp" make ci-fast-extra"#,
        "make TMPDIR=1 ci-fast",
        r#""TMPDIR=1" make ci-fast"#,
    ] {
        cases.push((command, pass(command), unestablished()));
    }
    // Only the superseded record names the host command; the replacing pass
    // establishes it through their shared identity, in either report order.
    let note = Some("the runner environment was corrected");
    let replaced = vec![
        record(FAST, Some("host-ci"), Failed, Superseded, note),
        required("make ci-fast --locked", Some("host-ci"), true),
    ];
    let reversed = replaced.iter().rev().cloned().collect();
    cases.push(("same-check replacement, attempt first", replaced, Ok(())));
    cases.push(("same-check replacement, pass first", reversed, Ok(())));
    for (name, records, expected) in cases {
        assert_eq!(validation_evidence(&records, &context), expected, "{name}");
        let certificate = certificate_requiring(records, &host_required);
        assert_consumer_agrees(&certificate, &expected, name);
    }

    // Neither boundary reads a missing host snapshot as an empty list.
    let mut legacy = certificate(scoped_passes());
    legacy.required_validation_commands = None;
    let observed = certificate_acceptable(&legacy);
    assert_eq!(observed, Err(ValidationContractMissing));
    let snapshotless = ValidationContext {
        required_validation_commands: None,
        ..scoped()
    };
    let observed = validation_evidence(&scoped_passes(), &snapshotless);
    assert_eq!(observed, Err(ValidationDefect::HostContractMissing));
}

const AFFECTED: &str = "make ci-test-affected";

/// ORB-14684's reviewer filed its failed `make ci-test-affected` run as a
/// diagnostic whose sources lie outside the candidate.
fn incident_records() -> Vec<ReviewValidation> {
    let command = format!("CI_TEST_BASE=8d9d8b98 {AFFECTED}");
    scoped_passes_and(diagnostic(&command, Failed, &[UNRELATED_FIXTURE]))
}

/// The incident shape [ORB-14684]: the reviewer filed a failed
/// `make ci-test-affected` run, which the owner lists in
/// `review.baseline_commands`, as a `diagnostic` whose self-reported sources
/// all lie outside the candidate. The owner trusts that gate, so the
/// reviewer's sources cannot excuse it: it passes, or carries a baseline
/// claim settlement reproduced on the pinned base. The neighbouring outcomes
/// the rule leaves as they were: an unlisted diagnostic, and required or
/// passing records of the listed gate.
#[test]
fn a_failed_trusted_gate_filed_as_diagnostic_is_never_coverage() {
    let baseline = vec![AFFECTED.to_string()];
    let context = ValidationContext {
        baseline_commands: &baseline,
        ..scoped()
    };
    let defect = validation_evidence(&incident_records(), &context)
        .expect_err("a listed gate's failure is not the reviewer's to excuse");
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
    // The certificate's own snapshot applies the same rule to its consumer.
    let certificate = certificate_trusting(incident_records(), &baseline);
    assert!(exclusion(&exact_delivery(), &certificate, &facts()).is_err());

    let incident = format!("CI_TEST_BASE=8d9d8b98 {AFFECTED}");
    let wrapped = "set -o pipefail; make ci-test-affected | tee log";
    let mut wrapped_record = diagnostic(wrapped, Failed, &[UNRELATED_FIXTURE]);
    wrapped_record.check = Some(AFFECTED.into());
    let unlisted = diagnostic(WORKSPACE, Failed, &[UNRELATED_FIXTURE]);
    for (name, record, expected) in [
        (
            "the incident",
            incident_records().pop().unwrap(),
            Err(trusted_diagnostic(&incident)),
        ),
        (
            "a `check` identity naming the listed command is the same gate",
            wrapped_record,
            Err(trusted_diagnostic(wrapped)),
        ),
        (
            "ORB-14192: an unlisted out-of-scope failure is an honest diagnostic",
            unlisted,
            Ok(()),
        ),
        (
            "a failed required record of the gate still blocks as a required failure",
            required(AFFECTED, None, false),
            Err(not_passed(AFFECTED, Failed)),
        ),
        (
            "a passing required record of the gate is unaffected",
            required(AFFECTED, None, true),
            Ok(()),
        ),
        (
            "a passing diagnostic of the gate is unaffected",
            diagnostic(AFFECTED, Passed, &[]),
            Ok(()),
        ),
    ] {
        let records = scoped_passes_and(record);
        assert_eq!(validation_evidence(&records, &context), expected, "{name}");
        let certificate = certificate_trusting(records, &baseline);
        assert_consumer_agrees(&certificate, &expected, name);
    }
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
    let observed = certificate_acceptable(&read);
    assert_eq!(observed, Ok(()), "judged exactly as when it was issued");

    // A failed diagnostic of a captured required command is refused with
    // or without the baseline snapshot.
    let mut host_diagnostic = scoped_passes();
    host_diagnostic.push(required(AFFECTED, None, true));
    host_diagnostic.push(diagnostic(FAST, Failed, &[UNRELATED_FIXTURE]));
    let issued = certificate_requiring(host_diagnostic, &[FAST.to_string()]);
    let observed = certificate_acceptable(&legacy(&issued));
    assert_eq!(observed, Err(ValidationIncomplete));

    let mut contractless = certificate(incident_records());
    contractless.required_validation_commands = None;
    let observed = certificate_acceptable(&legacy(&contractless));
    assert_eq!(observed, Err(ValidationContractMissing));
}

/// A Bubblewrap-gated test run inside an agent lane, whose own sandbox
/// refuses a nested one: the test printed its deferral and returned, so the
/// run passed without executing the confined path [ORB-14334].
const CONFINED: &str =
    "cargo test -p orbit-exec --test sandbox bwrap_child_gets_only_the_supplied_environment";
const DEFERRAL: &str = "DEFERRED: bubblewrap unavailable: \
     bwrap_child_gets_only_the_supplied_environment: No permissions to create new namespace";

fn deferred(passed: bool, notices: &[&str]) -> ReviewValidation {
    let mut record = required(CONFINED, None, passed);
    record.deferred = notices.iter().map(|notice| (*notice).to_string()).collect();
    record
}

/// A pass counts as executing its confined path only when it reported no
/// deferral; a failed run that also deferred is still a failure.
#[test]
fn a_pass_that_deferred_its_confined_path_never_counts_as_executing_it() {
    let lane_run = deferred(true, &[DEFERRAL]);
    assert_eq!(lane_run.executed_outcome(), NotRun);
    let pass = || required(FAST, None, true);
    let (command, notice) = (CONFINED.to_string(), DEFERRAL.to_string());
    let deferred_only = ValidationDefect::RequiredDeferred { command, notice };
    for (name, records, expected) in [
        (
            "a certificate cannot spend a deferred-only pass as coverage",
            vec![pass(), lane_run.clone()],
            Err(deferred_only),
        ),
        (
            "nor does it replace a superseded attempt of the same check",
            vec![pass(), superseded(CONFINED, None), lane_run],
            Err(unreplaced(CONFINED)),
        ),
        (
            "a failed run that also deferred is a failure, not a deferral",
            vec![deferred(false, &[DEFERRAL])],
            Err(not_passed(CONFINED, Failed)),
        ),
        ("no deferral", vec![pass(), deferred(true, &[])], Ok(())),
        (
            "only blank notices",
            vec![pass(), deferred(true, &["", "  "])],
            Ok(()),
        ),
    ] {
        assert_eq!(default_evidence(&records), expected, "{name}");
        assert_consumer_agrees(&certificate(records), &expected, name);
    }
}

/// [ORB-15083] A diagnostic that never ran is a skipped check under the wrong
/// role. The reviewer can fix that without rerunning anything, so it is
/// correctable, and the correction depends on what the owner makes of the
/// command. A denied diagnostic and a passed control stay observations.
#[test]
fn a_not_run_diagnostic_is_correctable_with_the_remedy_of_its_class() {
    use SkippedCheck::{Baseline, HostRequired, Unlisted};
    const GOLDENS: &str = "make goldens";
    let host_required = [LINT.to_string()];
    let baseline = [GOLDENS.to_string(), LINT.to_string()];
    let context = ValidationContext {
        required_validation_commands: Some(&host_required),
        baseline_commands: &baseline,
        ..ValidationContext::default()
    };
    let passing = required(FAST, None, true);
    for (command, class, remedy) in [
        (LINT, HostRequired, "run it and record it `required`"),
        (GOLDENS, Baseline, "record it `excluded` or omit it"),
        (CODEQL, Unlisted, "never record it `excluded`"),
    ] {
        let records = [passing.clone(), diagnostic(command, NotRun, &[])];
        let defect = validation_evidence(&records, &context).expect_err(command);
        let expected = ValidationDefect::DiagnosticNotRun {
            command: command.into(),
            class,
        };
        assert_eq!(defect, expected);
        let reason = defect.reason();
        assert!(defect.correctable(), "{reason}");
        let skipped = format!("`{command}` was recorded as diagnostic but is not_run");
        assert!(
            reason.starts_with("validation_contradicted:")
                && reason.contains(&skipped)
                && reason.contains(remedy),
            "{reason}"
        );
    }

    for (name, observation, defect) in [
        (
            "a denied diagnostic",
            diagnostic(CODEQL, Denied, &[]),
            contradicted(CODEQL, Diagnostic, Denied),
        ),
        (
            "a control that never ran",
            control(CODEQL, PreFix, NotRun, &[IN_SCOPE]),
            contradicted(CODEQL, ExpectedFailure, NotRun),
        ),
    ] {
        let observed = scoped_evidence(&[passing.clone(), observation]).expect_err(name);
        assert_eq!(observed, defect, "{name}");
        assert!(!observed.correctable(), "{name}: {}", observed.reason());
    }

    // The relabel the correction asks for establishes the candidate, and a
    // host-required command recorded excluded does not.
    let relabelled = |command: &str| {
        let mut skipped = diagnostic(command, NotRun, &[]);
        skipped.role = Excluded;
        let mut records = vec![passing.clone(), skipped];
        if command != LINT {
            records.push(required(LINT, None, true));
        }
        validation_evidence(&records, &context)
    };
    assert_eq!(relabelled(GOLDENS), Ok(()));
    assert_eq!(relabelled(LINT), Err(host_unestablished(LINT)));
}

/// [ORB-15546] A failed required check the host verified known red counts as
/// coverage only through the certificate's own entry, and keeps its failed
/// outcome. The entry must carry the record's id and host command, name a
/// test, and have been verified on the certificate's base and final
/// candidate; a record without a baseline claim or one that never ran is not
/// what the host verified.
#[test]
fn a_known_red_required_check_counts_only_through_its_bound_host_entry() {
    const GATE: &str = "make ci-test-affected";
    let failed = || {
        let mut record = with_id(required(GATE, None, false), "V1");
        record.baseline = Some(ReviewBaselineClaim {
            base_commit: "commit-base".into(),
            outcome: Failed,
            failures: vec!["suite::broken".into()],
        });
        record
    };
    let known = || HostKnownRed {
        record_id: Some("V1".into()),
        validation: KnownRedValidation {
            command: GATE.into(),
            base_sha: "commit-base".into(),
            tested_head: "commit-final".into(),
            run_id: "jrun-settle".into(),
            failures: BaselineRedFailures::new(["test suite::broken".to_string()], ""),
        },
        evidence_artifact: REVIEW_BASELINE_ARTIFACT.into(),
    };
    let issued = |record: ReviewValidation, known_red: Vec<HostKnownRed>| {
        let mut certificate = certificate_requiring(vec![record], &[GATE.to_string()]);
        certificate.known_red = known_red;
        certificate
    };

    let accepted = issued(failed(), vec![known()]);
    assert_eq!(certificate_acceptable(&accepted), Ok(()));
    assert_eq!(accepted.validation[0].outcome, Failed);

    let mutated = |change: fn(&mut HostKnownRed)| {
        let mut entry = known();
        change(&mut entry);
        issued(failed(), vec![entry])
    };
    let mut unclaimed = failed();
    unclaimed.baseline = None;
    let mut never_ran = failed();
    never_ran.outcome = NotRun;
    let cases = [
        ("no entry", issued(failed(), Vec::new())),
        (
            "another candidate",
            mutated(|entry| entry.validation.tested_head = "commit-other".into()),
        ),
        (
            "another base",
            mutated(|entry| entry.validation.base_sha = "commit-other".into()),
        ),
        (
            "another record",
            mutated(|entry| entry.record_id = Some("V2".into())),
        ),
        (
            "another command",
            mutated(|entry| entry.validation.command = FAST.into()),
        ),
        (
            "no test named",
            mutated(|entry| entry.validation.failures = BaselineRedFailures::default()),
        ),
        ("no baseline claim", issued(unclaimed, vec![known()])),
        ("never ran", issued(never_ran, vec![known()])),
    ];
    for (name, certificate) in cases {
        let observed = certificate_acceptable(&certificate);
        assert_eq!(observed, Err(ValidationIncomplete), "{name}");
    }
}
