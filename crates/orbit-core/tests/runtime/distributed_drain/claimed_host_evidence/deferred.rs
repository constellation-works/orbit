//! [ORB-17043] A broader required run that passed only by deferring the tests
//! its reviewer also names as `host_sandbox_test` requirements.
//!
//! The shape is a macOS claimed leaf's: the reviewer ran `cargo test` over a
//! module, two of its tests printed `DEFERRED: ps denied:` and returned, and
//! the reviewer named each as a host requirement filtered to exactly that
//! test. The claim host replays both; when they pass they establish the
//! broader record, and anything short of that keeps it short.

use super::*;

use orbit_types::workflow::{FindingDisposition, ReviewEvidenceHold, ReviewFinding};

const BROAD: &str = "cargo test -p orbit-exec --test probe fixture_children -- --nocapture";
const A: &str = "fixture_children::a_killed_parent_takes_its_group_with_it";
const B: &str = "fixture_children::an_overrunning_child_is_killed";
const C: &str = "fixture_children::a_third_child_is_reaped";
const SEATBELT_CONFORMANCE_TABLE: &str =
    "a_config_templated_write_root_outside_scratch_needs_fs_consent_scenarios";

/// The candidate's workspace for ORB-29029: `orbit-core`'s `runtime` target
/// declares `decision_raise`, whose `scenario_table!` runs
/// `managed_raise_keeps_shared_root_read_only`.
const SCENARIO_WORKSPACE: [(&str, &str); 4] = [
    (
        "Cargo.toml",
        "[workspace]\nmembers = [\"crates/orbit-core\"]\nresolver = \"3\"\n",
    ),
    (
        "crates/orbit-core/Cargo.toml",
        "[package]\nname = \"orbit-core\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
    ),
    (
        "crates/orbit-core/tests/runtime/main.rs",
        "mod decision_raise;\n",
    ),
    (
        "crates/orbit-core/tests/runtime/decision_raise.rs",
        "crate::scenario_table!(managed_scenarios, managed_raise_keeps_shared_root_read_only);\n",
    ),
];

const SCENARIO_ROW: &str = "managed_raise_keeps_shared_root_read_only";

const SCENARIO_NATIVE: &str =
    "cargo test -p orbit-core --test runtime decision_raise::managed_scenarios";

const SCENARIO_NEXTEST: &str = "cargo nextest run -p orbit-core -p orbit-exec --test runtime";

/// The host requirement that runs exactly `test`.
fn exact(test: &str) -> String {
    format!("cargo test -p orbit-exec --test probe {test}")
}

fn notice(test: &str) -> String {
    format!("DEFERRED: ps denied: {test}: ps -o lstart= -p 1: Operation not permitted")
}

fn artifact(index: usize) -> String {
    format!("evidence/host-{index}.json")
}

fn log_artifact(index: usize) -> String {
    format!("evidence/host-{index}.log.json")
}

/// The candidate's probe: it runs the one test its filter names and reports
/// libtest's summary, or does what `cases` gives for that filter instead.
fn probe(cases: &[(&str, &str)]) -> String {
    let arms = cases
        .iter()
        .map(|(test, body)| format!("  {test}) {body} ;;\n"))
        .collect::<String>();
    format!(
        "case \"$1\" in\n{arms}  *) echo \"ORBIT_HOST_PROBE_RAN $1 $(git rev-parse HEAD)\"\n     \
         echo \"test $1 ... ok\"\n     \
         echo 'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out' ;;\n\
         esac\n"
    )
}

/// What the reviewer of ORB-17001 reported: `make ci-fast` passed, `BROAD`
/// passed with an `outcome` and a deferral notice for each of `deferred`, and
/// each `(command, os)` of `hosts` is a `host_sandbox_test` requirement with
/// its own `not_run` record when `narrow`.
struct Deferred<'a> {
    broad: String,
    deferred: &'a [&'a str],
    hosts: Vec<(String, EvidenceHostOs)>,
    narrow: bool,
    outcome: ValidationOutcome,
    findings: Vec<ReviewFinding>,
}

impl<'a> Deferred<'a> {
    fn covering(deferred: &'a [&'a str]) -> Self {
        Self {
            broad: BROAD.into(),
            deferred,
            hosts: deferred
                .iter()
                .map(|test| (exact(test), host_os()))
                .collect(),
            narrow: true,
            outcome: ValidationOutcome::Passed,
            findings: Vec::new(),
        }
    }

    fn report(self, leaf: &ReviewedLeaf, attempt_id: &str) {
        let record = |id: String, command: &str, outcome, deferred: Vec<String>| ReviewValidation {
            id: Some(id),
            command: command.into(),
            outcome,
            role: ValidationRole::Required,
            note: None,
            check: None,
            control: None,
            sources: Vec::new(),
            mutation_target: Vec::new(),
            repaired_blob: Default::default(),
            deferred,
            baseline: None,
        };
        let mut validation = vec![
            record(
                "V1".into(),
                "make ci-fast",
                ValidationOutcome::Passed,
                Vec::new(),
            ),
            record(
                "V2".into(),
                &self.broad,
                self.outcome,
                self.deferred.iter().map(|test| notice(test)).collect(),
            ),
        ];
        if self.narrow {
            for (index, (command, _)) in self.hosts.iter().enumerate() {
                validation.push(record(
                    format!("V{}", index + 3),
                    command,
                    ValidationOutcome::NotRun,
                    Vec::new(),
                ));
            }
        }
        leaf.put_report(&ReviewReport {
            external_evidence: self
                .hosts
                .iter()
                .enumerate()
                .map(|(index, (command, os))| ReviewEvidenceRequirement {
                    kind: ReviewEvidenceKind::HostSandboxTest,
                    name: format!("Deferred test {index}"),
                    command: command.clone(),
                    artifact: artifact(index),
                    os: Some(*os),
                    hosted_check: None,
                })
                .collect(),
            schema_version: REVIEW_CONTRACT_VERSION,
            attempt_id: attempt_id.into(),
            verdict: if self.outcome == ValidationOutcome::Failed {
                ReviewVerdict::Reject
            } else {
                ReviewVerdict::Incomplete
            },
            summary: "Checked the change; the deferred tests need the host.".into(),
            findings: self.findings,
            validation,
            retired_validation: Vec::new(),
            escalation: Some("the deferred tests need the host".into()),
        });
    }
}

fn broad_record<'a>(certificate: &'a ReviewCertificate, command: &str) -> &'a ReviewValidation {
    certificate
        .validation
        .iter()
        .find(|record| record.command == command)
        .expect("the broader record stays on the certificate")
}

/// Nothing ran on the host: no host record and no log for any requirement.
fn assert_no_replay(leaf: &ReviewedLeaf, case: &str, requirements: usize) {
    let certificate = certificate(leaf);
    assert!(
        certificate.host_evidence.is_empty(),
        "{case}: {:?}",
        certificate.host_evidence
    );
    for index in 0..requirements {
        assert!(
            leaf.owner_artifact(&log_artifact(index)).is_none(),
            "{case}"
        );
    }
    assert!(!certificate.verdict.passed(), "{case}");
}

/// The ORB-17001 shape: both replays run outside the agent sandbox at the
/// final candidate, establish the broader record, and the review passes with
/// both host records on the certificate. The handoff pins both results and
/// logs, and the owner refuses it without them.
fn a_broader_run_deferring_only_host_tests_settles_on_their_replays() {
    settles_on_their_replays(BROAD);
}

/// [ORB-17106] A broader run with a benign option next to its one target
/// (`--locked`, `--no-fail-fast`) names the same target, so its deferrals
/// settle on the same replays as the plain shape.
fn a_broader_run_with_non_selecting_options_settles_on_its_replays() {
    for broad in [
        "cargo test -p orbit-exec --locked --test probe fixture_children -- --nocapture",
        "cargo test --no-fail-fast -p orbit-exec --test probe fixture_children -- --nocapture",
    ] {
        settles_on_their_replays(broad);
    }
}

fn settles_on_their_replays(broad_command: &str) {
    let (leaf, attempt_id) = leaf_with_probe(&probe(&[]));
    Deferred {
        broad: broad_command.into(),
        ..Deferred::covering(&[A, B])
    }
    .report(&leaf, &attempt_id);

    let settled = leaf
        .settle()
        .unwrap_or_else(|refused| panic!("{broad_command}: {refused}"));
    assert_eq!(settled["gate"], "passed", "{broad_command}: {settled}");
    assert!(leaf.owner_artifact(REVIEW_EVIDENCE_HOLD_ARTIFACT).is_none());
    let repo = &leaf.pair.follower_repo;
    let head = git(repo, &["rev-parse", "HEAD"]).trim().to_string();
    let tree = git(repo, &["rev-parse", "HEAD^{tree}"]).trim().to_string();
    let certificate = certificate(&leaf);
    assert!(certificate.verdict.passed(), "{:?}", certificate.verdict);
    assert_eq!(
        certificate
            .host_evidence
            .iter()
            .map(|record| (record.command.as_str(), record.passed, record.tree.as_str()))
            .collect::<Vec<_>>(),
        [
            (exact(A).as_str(), true, tree.as_str()),
            (exact(B).as_str(), true, tree.as_str())
        ]
    );
    let broad = broad_record(&certificate, broad_command);
    assert_eq!(broad.executed_outcome(), ValidationOutcome::Passed);
    assert!(broad.deferred.is_empty(), "{broad:?}");
    for (index, test) in [A, B].into_iter().enumerate() {
        let log: Value =
            serde_json::from_slice(&leaf.owner_artifact(&log_artifact(index)).unwrap()).unwrap();
        assert!(
            log["output"]
                .as_str()
                .unwrap_or_default()
                .lines()
                .any(|line| line == format!("ORBIT_HOST_PROBE_RAN {test} {head}")),
            "the host executed {test} at the final candidate: {log:#}"
        );
    }
    let evidence: HandoffReviewEvidence =
        serde_json::from_value(settled["handoff_evidence"].clone()).unwrap();
    assert_eq!(
        evidence
            .host_evidence
            .iter()
            .map(|pinned| pinned.path.clone())
            .collect::<Vec<_>>(),
        [artifact(0), log_artifact(0), artifact(1), log_artifact(1)]
    );
    let head = SourceRevision { commit: head, tree };
    let mut unpinned = evidence.clone();
    unpinned.host_evidence.truncate(2);
    leaf.owner_accepts(unpinned, &head)
        .expect_err("every host result and log is pinned");
    assert_eq!(leaf.pair.owner_status(&leaf.task), "in-progress");
    leaf.owner_accepts(evidence, &head)
        .expect("the owner accepts the handoff with both host runs");
    assert_eq!(leaf.pair.owner_status(&leaf.task), "review");
}

/// The nested-Seatbelt conformance table emits the exact notice the reviewer
/// turns into a host requirement; settlement runs that filtered table on the
/// claimed host and passes the review without escalation [ORB-19501].
fn a_nested_seatbelt_conformance_deferral_settles_on_its_host_replay() {
    let filter = format!("admission_audit::plugin_conformance::{SEATBELT_CONFORMANCE_TABLE}");
    let output = std::process::Command::new(std::env::current_exe().expect("runtime test binary"))
        .args([
            "--exact",
            filter.as_str(),
            "--nocapture",
            "--test-threads=1",
        ])
        .env("ORBIT_TEST_FORCE_SEATBELT_REFUSAL", "1")
        .env_remove("ORBIT_REQUIRE_SANDBOX_EXEC")
        .output()
        .expect("run the conformance table under a forced Seatbelt refusal");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "the nested-Seatbelt table defers instead of failing:\n{stdout}\n{stderr}"
    );
    let prefix = orbit_types::workflow::SEATBELT_DEFERRAL_PREFIX;
    let deferred = stderr
        .lines()
        .filter(|line| line.starts_with(prefix))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    assert_eq!(deferred.len(), 3, "every conformance row defers: {stderr}");
    assert!(
        deferred
            .iter()
            .all(|line| line.starts_with(&format!("{prefix} {SEATBELT_CONFORMANCE_TABLE}:"))),
        "the deferrals name the filtered table the host replays: {deferred:?}"
    );
    assert!(
        !stderr.contains(
            "qa-scenario-pass: a_config_templated_write_root_outside_scratch_needs_fs_consent"
        ) && !stderr.contains(
            "qa-scenario-pass: a_workspace_exact_file_grant_passes_conformance_on_a_created_file"
        ) && !stderr.contains(
            "qa-scenario-pass: goldens_supply_fixture_secrets_and_the_host_store_is_never_read"
        ),
        "a deferred row gets no pass credit: {stderr}"
    );

    let command = format!("cargo test -p orbit-core --test runtime {SEATBELT_CONFORMANCE_TABLE}");
    let (leaf, attempt_id) = leaf_with_probe_target(&probe(&[]), "runtime");
    leaf.put_report(&ReviewReport {
        external_evidence: vec![ReviewEvidenceRequirement {
            kind: ReviewEvidenceKind::HostSandboxTest,
            name: "Nested-Seatbelt conformance table".into(),
            command: command.clone(),
            artifact: artifact(0),
            os: Some(host_os()),
            hosted_check: None,
        }],
        schema_version: REVIEW_CONTRACT_VERSION,
        attempt_id: attempt_id.clone(),
        verdict: ReviewVerdict::Incomplete,
        summary: "The nested-Seatbelt conformance rows need the claimed host.".into(),
        findings: Vec::new(),
        validation: vec![
            ReviewValidation {
                id: Some("V1".into()),
                command: "make ci-fast".into(),
                outcome: ValidationOutcome::Passed,
                role: ValidationRole::Required,
                note: None,
                check: None,
                control: None,
                sources: Vec::new(),
                mutation_target: Vec::new(),
                repaired_blob: Default::default(),
                deferred: Vec::new(),
                baseline: None,
            },
            ReviewValidation {
                id: Some("V2".into()),
                command: command.clone(),
                outcome: ValidationOutcome::Passed,
                role: ValidationRole::Required,
                note: Some("The nested-Seatbelt table emitted these deferrals.".into()),
                check: None,
                control: None,
                sources: Vec::new(),
                mutation_target: Vec::new(),
                repaired_blob: Default::default(),
                deferred,
                baseline: None,
            },
        ],
        retired_validation: Vec::new(),
        escalation: Some("the nested-Seatbelt table needs the claimed host".into()),
    });

    let settled = leaf
        .settle()
        .expect("the claimed host replays the deferred table and settles the review");
    assert_eq!(settled["gate"], "passed", "{settled}");
    assert!(leaf.owner_artifact(REVIEW_EVIDENCE_HOLD_ARTIFACT).is_none());
    let head = git(&leaf.pair.follower_repo, &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    let tree = git(&leaf.pair.follower_repo, &["rev-parse", "HEAD^{tree}"])
        .trim()
        .to_string();
    let certificate = certificate(&leaf);
    assert_eq!(certificate.attempt_id, attempt_id);
    assert!(certificate.verdict.passed(), "{:?}", certificate.verdict);
    assert!(
        certificate.escalation.is_none(),
        "{:?}",
        certificate.escalation
    );
    let [host] = certificate.host_evidence.as_slice() else {
        panic!("one host replay: {:?}", certificate.host_evidence);
    };
    assert!(host.passed, "{host:?}");
    assert_eq!(host.command, command);
    assert_eq!(host.tree, tree);
    let broad = broad_record(&certificate, &command);
    assert_eq!(broad.executed_outcome(), ValidationOutcome::Passed);
    assert!(broad.deferred.is_empty(), "{broad:?}");
    let log: Value = serde_json::from_slice(
        &leaf
            .owner_artifact(&log_artifact(0))
            .expect("host replay log"),
    )
    .unwrap();
    assert!(
        log["output"]
            .as_str()
            .unwrap_or_default()
            .lines()
            .any(|line| {
                line == format!("ORBIT_HOST_PROBE_RAN {SEATBELT_CONFORMANCE_TABLE} {head}")
            }),
        "the host replay ran the exact table at the final candidate: {log:#}"
    );
}

/// A deferral no host requirement runs exactly, an open finding, a failed
/// broader run, a requirement outside the admitted grammar or without a
/// record of its own (the owner binds each host result to one), or a moved
/// candidate: the host runs nothing, and the broader record stays short.
fn a_broader_run_the_host_cannot_establish_runs_no_replay() {
    let covering = || Deferred::covering(&[A, B]);
    let mut cases = vec![
        (
            "an uncovered deferral",
            Deferred {
                hosts: vec![(exact(A), host_os()), (exact(B), host_os())],
                ..Deferred::covering(&[A, B, C])
            },
        ),
        (
            "an open finding",
            Deferred {
                findings: vec![ReviewFinding {
                    id: "F1".into(),
                    severity: "medium".into(),
                    summary: "The group is not killed".into(),
                    paths: vec!["src/f0.rs".into()],
                    disposition: FindingDisposition::Open,
                    change: None,
                }],
                ..covering()
            },
        ),
        (
            "a failed broader run",
            Deferred {
                outcome: ValidationOutcome::Failed,
                ..covering()
            },
        ),
        (
            "requirements without records of their own",
            Deferred {
                narrow: false,
                ..covering()
            },
        ),
    ];
    for (case, command) in [
        (
            "no filter",
            "cargo test -p orbit-exec --test probe".to_string(),
        ),
        ("a module filter", exact("fixture_children")),
        (
            "another package",
            format!("cargo test -p orbit-common --test probe {B}"),
        ),
        ("argument_not_allowed", format!("{} --exact", exact(B))),
        ("shell_metacharacter", format!("{};touch ran", exact(B))),
        ("command_not_allowed", format!("cargo nextest run {B}")),
    ] {
        cases.push((
            case,
            Deferred {
                hosts: vec![(exact(A), host_os()), (command, host_os())],
                ..covering()
            },
        ));
    }
    for selector in [
        "--lib",
        "--bins",
        "--bin orbit",
        "--tests",
        "--examples",
        "--example demo",
        "--benches",
        "--bench perf",
        "--all-targets",
        "--doc",
        "--workspace",
        "--all",
    ] {
        cases.push((
            selector,
            Deferred {
                broad: format!(
                    "cargo test -p orbit-exec {selector} --test probe fixture_children -- --nocapture"
                ),
                ..covering()
            },
        ));
    }
    let requirements = |deferred: &Deferred| deferred.hosts.len();
    for (case, deferred) in cases {
        let (leaf, attempt_id) = leaf_with_probe(&probe(&[]));
        let count = requirements(&deferred);
        let failed = deferred.outcome == ValidationOutcome::Failed;
        let broad_command = deferred.broad.clone();
        deferred.report(&leaf, &attempt_id);
        let refused = leaf.settle().expect_err(case);
        assert!(refused.contains("review_gate_blocked"), "{case}: {refused}");
        assert_no_replay(&leaf, case, count);
        let broad = broad_record(&certificate(&leaf), &broad_command).clone();
        let expected = if failed {
            ValidationOutcome::Failed
        } else {
            ValidationOutcome::NotRun
        };
        assert_eq!(broad.executed_outcome(), expected, "{case}");
        assert!(!leaf.pair.follower_repo.join("ran").exists(), "{case}");
    }

    let (leaf, attempt_id) = leaf_with_probe(&probe(&[]));
    Deferred::covering(&[A, B]).report(&leaf, &attempt_id);
    let repo = &leaf.pair.follower_repo;
    std::fs::write(repo.join("src/f0.rs"), "fn work() {}\n// moved\n").unwrap();
    git(repo, &["commit", "-qam", "Move the candidate"]);
    let refused = leaf.settle().expect_err("a moved candidate is refused");
    assert!(refused.contains("candidate_changed"), "{refused}");
    for index in 0..2 {
        assert!(leaf.owner_artifact(&log_artifact(index)).is_none());
    }
}

/// A replay that does not execute its test leaves that deferral open and the
/// review holds for it; one that fails fails its own record and the broader
/// one, and the review blocks. The other replay's pass counts only for its
/// own test.
fn a_replay_short_of_executing_its_test_leaves_the_broader_run_short() {
    let failing = "echo \"test $1 ... FAILED\"; \
        echo 'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out'; \
        exit 101";
    for (body, reason) in [
        (
            "echo 'SKIP: the probe needs a kernel feature'; \
             echo 'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out'",
            HostEvidenceReason::SelfSkipped,
        ),
        (
            "echo 'sandbox-exec: sandbox_apply: Operation not permitted'; \
             echo 'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out'",
            HostEvidenceReason::SandboxUnavailable,
        ),
        (
            "echo 'test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 3 filtered out'",
            HostEvidenceReason::NoTestsRan,
        ),
        (failing, HostEvidenceReason::TestFailed),
    ] {
        let case = reason.as_str();
        let (leaf, attempt_id) = leaf_with_probe(&probe(&[(B, body)]));
        Deferred::covering(&[A, B]).report(&leaf, &attempt_id);
        let refused = leaf.settle().expect_err(case);
        let certificate = certificate(&leaf);
        assert!(!certificate.verdict.passed(), "{case}");
        let outcomes = certificate
            .host_evidence
            .iter()
            .map(|record| (record.command.clone(), record.passed, record.reason))
            .collect::<Vec<_>>();
        assert_eq!(
            outcomes,
            [(exact(A), true, None), (exact(B), false, Some(reason))],
            "{case}"
        );
        assert!(leaf.owner_artifact(&artifact(1)).is_none(), "{case}");
        let broad = broad_record(&certificate, BROAD);
        if reason == HostEvidenceReason::TestFailed {
            assert!(refused.contains("review_gate_blocked"), "{case}: {refused}");
            assert_eq!(broad.outcome, ValidationOutcome::Failed, "{case}");
            let narrow = certificate
                .validation
                .iter()
                .find(|record| record.command == exact(B))
                .unwrap();
            assert_eq!(narrow.outcome, ValidationOutcome::Failed, "{case}");
            assert!(leaf.owner_artifact(REVIEW_EVIDENCE_HOLD_ARTIFACT).is_none());
        } else {
            assert!(
                refused.contains("review_awaiting_evidence"),
                "{case}: {refused}"
            );
            assert_eq!(
                broad.executed_outcome(),
                ValidationOutcome::NotRun,
                "{case}"
            );
            assert_eq!(broad.deferred, [notice(B)], "{case}: A's pass counts for A");
            let hold: ReviewEvidenceHold = serde_json::from_slice(
                &leaf.owner_artifact(REVIEW_EVIDENCE_HOLD_ARTIFACT).unwrap(),
            )
            .unwrap();
            assert_eq!(
                hold.requirements
                    .iter()
                    .map(|required| required.command.as_str())
                    .collect::<Vec<_>>(),
                [exact(B)],
                "{case}"
            );
        }
    }
}

/// A requirement for another OS is refused on this host and never run; the
/// review holds for it while this host's replay counts for its own test.
fn a_deferral_owed_to_another_os_holds_the_review_for_it() {
    let other = if host_os() == EvidenceHostOs::Linux {
        EvidenceHostOs::Macos
    } else {
        EvidenceHostOs::Linux
    };
    let (leaf, attempt_id) = leaf_with_probe(&probe(&[]));
    Deferred {
        hosts: vec![(exact(A), host_os()), (exact(B), other)],
        ..Deferred::covering(&[A, B])
    }
    .report(&leaf, &attempt_id);
    let held = leaf.settle().expect_err("B is owed to another host");
    assert!(held.contains("review_awaiting_evidence"), "{held}");
    let certificate = certificate(&leaf);
    let [ran, refused] = certificate.host_evidence.as_slice() else {
        panic!("{:?}", certificate.host_evidence);
    };
    assert!(ran.passed, "{ran:?}");
    assert_eq!(refused.reason, Some(HostEvidenceReason::OsMismatch));
    assert!(refused.host_command.is_none(), "{refused:?}");
    assert!(leaf.owner_artifact(&log_artifact(1)).is_none());
    assert_eq!(broad_record(&certificate, BROAD).deferred, [notice(B)]);
}

fn scenario_record(
    id: &str,
    command: &str,
    outcome: ValidationOutcome,
    deferred: Vec<String>,
) -> ReviewValidation {
    ReviewValidation {
        id: Some(id.into()),
        command: command.into(),
        outcome,
        role: ValidationRole::Required,
        note: None,
        check: None,
        control: None,
        sources: Vec::new(),
        mutation_target: Vec::new(),
        repaired_blob: Default::default(),
        deferred,
        baseline: None,
    }
}

/// Settle a nextest record that passed with a Bubblewrap deferral naming
/// [`SCENARIO_ROW`] on a candidate whose tree is `workspace`, and return the
/// certificate. Nothing replays: no host run may stand for the row.
fn settle_scenario_deferral(workspace: &[(&str, &str)]) -> ReviewCertificate {
    let (leaf, attempt_id) = leaf_with_candidate_files(&probe(&[]), false, "probe", workspace);
    let notice =
        orbit_exec::bwrap_deferral_notice(SCENARIO_ROW, "the host cannot apply the sandbox");
    leaf.put_report(&ReviewReport {
        external_evidence: Vec::new(),
        schema_version: REVIEW_CONTRACT_VERSION,
        attempt_id,
        verdict: ReviewVerdict::Accept,
        summary: "Checked the change; the consolidated scenario deferred on this host.".into(),
        findings: Vec::new(),
        validation: vec![
            scenario_record("V1", "make ci-fast", ValidationOutcome::Passed, Vec::new()),
            scenario_record(
                "V2",
                SCENARIO_NEXTEST,
                ValidationOutcome::Passed,
                vec![notice],
            ),
        ],
        retired_validation: Vec::new(),
        escalation: None,
    });
    // The deferred row keeps the review from passing; the diagnostic is the outcome.
    let _ = leaf.settle();
    let certificate = certificate(&leaf);
    assert!(!certificate.verdict.passed(), "{:?}", certificate.verdict);
    assert!(
        certificate.host_evidence.is_empty(),
        "{:?}",
        certificate.host_evidence
    );
    certificate
}

/// [ORB-29076] A nextest record passes with a Bubblewrap deferral that names a
/// consolidated scenario row. The row runs inside `decision_raise::managed_scenarios`,
/// so the escalation names that libtest and the row's marker. It never offers
/// a Cargo filter on the row, which runs no test, and no replay runs for it.
fn a_consolidated_scenario_deferral_names_its_libtest_and_marker() {
    let certificate = settle_scenario_deferral(&SCENARIO_WORKSPACE);
    let escalation = certificate.escalation.as_deref().unwrap_or_default();
    assert!(
        orbit_types::workflow::HostSandboxCommand::admit(SCENARIO_NATIVE, &[]).is_ok(),
        "the native check is admissible"
    );
    assert!(
        escalation.contains(SCENARIO_NATIVE)
            && escalation.contains(&format!("qa-scenario-pass: {SCENARIO_ROW}")),
        "the escalation names the libtest and its marker: {escalation}"
    );
    assert!(
        !escalation.contains(&format!("--test runtime {SCENARIO_ROW}")),
        "a Cargo filter on the row runs no test and is never offered: {escalation}"
    );
}

/// [ORB-29076] When two libtests run the same row, no libtest is its home. The
/// escalation names the uncovered command and the fresh-review route, and it
/// names no native check and no marker-bearing libtest.
fn a_scenario_row_with_two_homes_names_only_the_uncovered_command() {
    let mut workspace = SCENARIO_WORKSPACE.to_vec();
    workspace[2] = (
        "crates/orbit-core/tests/runtime/main.rs",
        "mod decision_raise;\nmod other_raise;\n",
    );
    workspace.push((
        "crates/orbit-core/tests/runtime/other_raise.rs",
        "crate::scenario_table!(other_scenarios, managed_raise_keeps_shared_root_read_only);\n",
    ));
    let certificate = settle_scenario_deferral(&workspace);
    let escalation = certificate.escalation.as_deref().unwrap_or_default();
    assert!(
        escalation.contains("unreconciled_scenario_deferral")
            && escalation.contains(SCENARIO_NEXTEST)
            && escalation.contains("admit a fresh review"),
        "the uncovered command and the fresh-review route: {escalation}"
    );
    assert!(
        !escalation.contains(SCENARIO_NATIVE)
            && !escalation.contains("decision_raise::managed_scenarios")
            && !escalation.contains("other_scenarios"),
        "no libtest is named as the home of an ambiguous row: {escalation}"
    );
}

/// [ORB-29576] The real `orbit-core/tests/runtime` layout mixes `#[path]`
/// support and nested module mounts with plain-`mod` scenario modules, and
/// `deferred.rs` itself holds string-literal `scenario_table!(` fixtures.
/// Neither suppresses `decision_raise::managed_scenarios` or pollutes its
/// unique home.
fn a_target_with_path_support_and_fixture_strings_resolves_its_plain_home() {
    let workspace = [
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/orbit-core\"]\nresolver = \"3\"\n",
        ),
        (
            "crates/orbit-core/Cargo.toml",
            "[package]\nname = \"orbit-core\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
        ),
        (
            "crates/orbit-core/tests/support/admission.rs",
            "pub fn helper() {}\n",
        ),
        (
            "crates/orbit-core/tests/runtime/main.rs",
            "#[path = \"../support/admission.rs\"]\n\
             mod admission_test_support;\n\
             mod decision_raise;\n\
             mod distributed_drain;\n\
             mod task_delivery;\n",
        ),
        (
            "crates/orbit-core/tests/runtime/decision_raise.rs",
            "#[cfg(target_os = \"linux\")]\n\
             crate::scenario_table!(managed_scenarios, managed_raise_keeps_shared_root_read_only);\n",
        ),
        (
            "crates/orbit-core/tests/runtime/distributed_drain/mod.rs",
            "mod claimed_host_evidence;\n",
        ),
        (
            "crates/orbit-core/tests/runtime/distributed_drain/claimed_host_evidence.rs",
            "mod deferred;\n",
        ),
        (
            "crates/orbit-core/tests/runtime/distributed_drain/claimed_host_evidence/deferred.rs",
            "const FIXTURE_PLAIN: &str = \"crate::scenario_table!(managed_scenarios, managed_raise_keeps_shared_root_read_only);\\n\";\n\
             const FIXTURE_RAW: &str = r#\"crate::scenario_table!(other_scenarios, managed_raise_keeps_shared_root_read_only);\"#;\n\
             const QUOTE_CHAR: char = '\"';\n\
             // crate::scenario_table!(commented, managed_raise_keeps_shared_root_read_only);\n\
             /* /* nested */ crate::scenario_table!(block_commented, managed_raise_keeps_shared_root_read_only); */\n\
             crate::scenario_table!(deferred_scenarios, a_different_row);\n",
        ),
        (
            "crates/orbit-core/tests/runtime/task_delivery.rs",
            "#[path = \"source_repair.rs\"]\nmod source_repair;\n",
        ),
        (
            "crates/orbit-core/tests/runtime/source_repair.rs",
            "#[path = \"source_repair/association.rs\"]\nmod association;\n",
        ),
        (
            "crates/orbit-core/tests/runtime/source_repair/association.rs",
            "crate::scenario_table!(association_scenarios, an_association_row);\n",
        ),
    ];
    let certificate = settle_scenario_deferral(&workspace);
    let escalation = certificate.escalation.as_deref().unwrap_or_default();
    assert!(
        escalation.contains(SCENARIO_NATIVE)
            && escalation.contains(&format!("qa-scenario-pass: {SCENARIO_ROW}")),
        "plain-mod home resolves alongside #[path] mounts and fixture literals: {escalation}"
    );
    assert!(
        !escalation.contains(&format!("--test runtime {SCENARIO_ROW}")),
        "a Cargo filter on the row runs no test and is never offered: {escalation}"
    );
}

/// [ORB-29576] When a `#[path]`-mounted module runs the same row as a
/// plain-`mod` module, the row still has no unique home.
fn a_scenario_row_shared_with_a_path_module_has_no_unique_home() {
    let mut workspace = SCENARIO_WORKSPACE.to_vec();
    workspace[2] = (
        "crates/orbit-core/tests/runtime/main.rs",
        "mod decision_raise;\n#[path = \"other_raise.rs\"]\nmod other_raise;\n",
    );
    workspace.push((
        "crates/orbit-core/tests/runtime/other_raise.rs",
        "crate::scenario_table!(other_scenarios, managed_raise_keeps_shared_root_read_only);\n",
    ));
    let certificate = settle_scenario_deferral(&workspace);
    let escalation = certificate.escalation.as_deref().unwrap_or_default();
    assert!(
        escalation.contains("unreconciled_scenario_deferral")
            && escalation.contains(SCENARIO_NEXTEST)
            && escalation.contains("admit a fresh review"),
        "the uncovered command and the fresh-review route: {escalation}"
    );
    assert!(
        !escalation.contains(SCENARIO_NATIVE)
            && !escalation.contains("decision_raise::managed_scenarios")
            && !escalation.contains("other_scenarios"),
        "no libtest is named as the home of a row shared with a #[path] module: {escalation}"
    );
}

/// [ORB-29577] Even when the reviewer's `external_evidence` names a zero-test
/// Cargo filter on the deferred scenario row (under `Accept` via
/// `synthesized_bubblewrap_requirements`, or `Incomplete` via `covers_deferral`),
/// settlement rejects matching the row to that filter, keeps the deferral
/// blocking, runs no host replay, and emits `unreconciled_scenario_deferral`.
fn a_reviewer_named_zero_test_scenario_filter_is_rejected() {
    let broad_runtime = "cargo test -p orbit-core --test runtime";
    let zero_test_filter = format!("{broad_runtime} {SCENARIO_ROW}");
    let notice =
        orbit_exec::bwrap_deferral_notice(SCENARIO_ROW, "the host cannot apply the sandbox");
    for (case, verdict, os, with_narrow_record) in [
        (
            "accept",
            ReviewVerdict::Accept,
            EvidenceHostOs::Linux,
            false,
        ),
        (
            "accept_with_narrow",
            ReviewVerdict::Accept,
            EvidenceHostOs::Linux,
            true,
        ),
        ("incomplete", ReviewVerdict::Incomplete, host_os(), true),
    ] {
        let (leaf, attempt_id) =
            leaf_with_candidate_files(&probe(&[]), false, "runtime", &SCENARIO_WORKSPACE);
        let mut validation = vec![
            scenario_record("V1", "make ci-fast", ValidationOutcome::Passed, Vec::new()),
            scenario_record(
                "V2",
                broad_runtime,
                ValidationOutcome::Passed,
                vec![notice.clone()],
            ),
        ];
        if with_narrow_record {
            validation.push(scenario_record(
                "V3",
                &zero_test_filter,
                ValidationOutcome::NotRun,
                Vec::new(),
            ));
        }
        leaf.put_report(&ReviewReport {
            external_evidence: vec![ReviewEvidenceRequirement {
                kind: ReviewEvidenceKind::HostSandboxTest,
                name: "Deferred scenario row".into(),
                command: zero_test_filter.clone(),
                artifact: artifact(0),
                os: Some(os),
                hosted_check: None,
            }],
            schema_version: REVIEW_CONTRACT_VERSION,
            attempt_id,
            verdict,
            summary: "Checked the change; the scenario row deferred.".into(),
            findings: Vec::new(),
            validation,
            retired_validation: Vec::new(),
            escalation: (verdict == ReviewVerdict::Incomplete)
                .then(|| "the deferred scenario row needs the host".into()),
        });

        let refused = leaf
            .settle()
            .expect_err(&format!("{case}: zero-test filter keeps deferral blocking"));
        assert!(refused.contains("review_gate_blocked"), "{case}: {refused}");
        assert!(
            leaf.owner_artifact(REVIEW_EVIDENCE_HOLD_ARTIFACT).is_none(),
            "{case}: no evidence hold may be created"
        );
        assert_no_replay(&leaf, case, 1);

        let certificate = certificate(&leaf);
        assert!(
            certificate.owed_evidence.is_empty(),
            "{case}: zero-test filter must not be owed: {:?}",
            certificate.owed_evidence
        );
        let broad = broad_record(&certificate, broad_runtime);
        assert_eq!(
            broad.deferred,
            std::slice::from_ref(&notice),
            "{case}: {broad:?}"
        );

        let escalation = certificate.escalation.as_deref().unwrap_or_default();
        assert!(
            escalation.contains("unreconciled_scenario_deferral")
                && escalation.contains(SCENARIO_NATIVE)
                && escalation.contains(&format!("qa-scenario-pass: {SCENARIO_ROW}")),
            "{case}: escalation names enclosing libtest and marker: {escalation}"
        );
        assert!(
            !escalation.contains(&format!("--test runtime {SCENARIO_ROW}")),
            "{case}: zero-test filter is never offered: {escalation}"
        );
    }

    // Ambiguous scenario row (two homes) also keeps the deferral blocking and
    // names only the uncovered command when the reviewer names the zero-test filter.
    let mut ambiguous_workspace = SCENARIO_WORKSPACE.to_vec();
    ambiguous_workspace[2] = (
        "crates/orbit-core/tests/runtime/main.rs",
        "mod decision_raise;\nmod other_raise;\n",
    );
    ambiguous_workspace.push((
        "crates/orbit-core/tests/runtime/other_raise.rs",
        "crate::scenario_table!(other_scenarios, managed_raise_keeps_shared_root_read_only);\n",
    ));
    let (leaf, attempt_id) =
        leaf_with_candidate_files(&probe(&[]), false, "runtime", &ambiguous_workspace);
    leaf.put_report(&ReviewReport {
        external_evidence: vec![ReviewEvidenceRequirement {
            kind: ReviewEvidenceKind::HostSandboxTest,
            name: "Deferred scenario row".into(),
            command: zero_test_filter.clone(),
            artifact: artifact(0),
            os: Some(host_os()),
            hosted_check: None,
        }],
        schema_version: REVIEW_CONTRACT_VERSION,
        attempt_id,
        verdict: ReviewVerdict::Incomplete,
        summary: "Checked the change; the ambiguous scenario row deferred.".into(),
        findings: Vec::new(),
        validation: vec![
            scenario_record("V1", "make ci-fast", ValidationOutcome::Passed, Vec::new()),
            scenario_record(
                "V2",
                broad_runtime,
                ValidationOutcome::Passed,
                vec![notice.clone()],
            ),
            scenario_record(
                "V3",
                &zero_test_filter,
                ValidationOutcome::NotRun,
                Vec::new(),
            ),
        ],
        retired_validation: Vec::new(),
        escalation: Some("the deferred scenario row needs the host".into()),
    });
    let refused = leaf
        .settle()
        .expect_err("ambiguous row with reviewer-named zero-test filter blocks");
    assert!(refused.contains("review_gate_blocked"), "{refused}");
    assert!(leaf.owner_artifact(REVIEW_EVIDENCE_HOLD_ARTIFACT).is_none());
    assert_no_replay(&leaf, "ambiguous_zero_test_filter", 1);
    let certificate = certificate(&leaf);
    let escalation = certificate.escalation.as_deref().unwrap_or_default();
    assert!(
        escalation.contains("unreconciled_scenario_deferral")
            && escalation.contains(broad_runtime)
            && escalation.contains("admit a fresh review"),
        "ambiguous row names uncovered command: {escalation}"
    );
    assert!(
        !escalation.contains(SCENARIO_NATIVE)
            && !escalation.contains(&format!("--test runtime {SCENARIO_ROW}")),
        "ambiguous row names neither enclosing libtest nor zero-test filter: {escalation}"
    );
}

crate::scenario_table!(
    a_broader_run_deferring_only_host_tests_scenarios,
    a_broader_run_deferring_only_host_tests_settles_on_their_replays,
    a_broader_run_with_non_selecting_options_settles_on_its_replays,
    a_broader_run_the_host_cannot_establish_runs_no_replay,
    a_replay_short_of_executing_its_test_leaves_the_broader_run_short,
    a_deferral_owed_to_another_os_holds_the_review_for_it,
    a_nested_seatbelt_conformance_deferral_settles_on_its_host_replay,
    a_consolidated_scenario_deferral_names_its_libtest_and_marker,
    a_scenario_row_with_two_homes_names_only_the_uncovered_command,
    a_target_with_path_support_and_fixture_strings_resolves_its_plain_home,
    a_scenario_row_shared_with_a_path_module_has_no_unique_home,
    a_reviewer_named_zero_test_scenario_filter_is_rejected
);
