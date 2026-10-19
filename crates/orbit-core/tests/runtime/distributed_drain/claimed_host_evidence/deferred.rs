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
    deferred: &'a [&'a str],
    hosts: Vec<(String, EvidenceHostOs)>,
    narrow: bool,
    outcome: ValidationOutcome,
    findings: Vec<ReviewFinding>,
}

impl<'a> Deferred<'a> {
    fn covering(deferred: &'a [&'a str]) -> Self {
        Self {
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
                BROAD,
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

fn broad_record(certificate: &ReviewCertificate) -> &ReviewValidation {
    certificate
        .validation
        .iter()
        .find(|record| record.command == BROAD)
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
    let (leaf, attempt_id) = leaf_with_probe(&probe(&[]));
    Deferred::covering(&[A, B]).report(&leaf, &attempt_id);

    let settled = leaf
        .settle()
        .expect("the host's replays complete the review");
    assert_eq!(settled["gate"], "passed", "{settled}");
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
    let broad = broad_record(&certificate);
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
    let requirements = |deferred: &Deferred| deferred.hosts.len();
    for (case, deferred) in cases {
        let (leaf, attempt_id) = leaf_with_probe(&probe(&[]));
        let count = requirements(&deferred);
        let failed = deferred.outcome == ValidationOutcome::Failed;
        deferred.report(&leaf, &attempt_id);
        let refused = leaf.settle().expect_err(case);
        assert!(refused.contains("review_gate_blocked"), "{case}: {refused}");
        assert_no_replay(&leaf, case, count);
        let broad = broad_record(&certificate(&leaf)).clone();
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
        let broad = broad_record(&certificate);
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
    assert_eq!(broad_record(&certificate).deferred, [notice(B)]);
}

crate::scenario_table!(
    a_broader_run_deferring_only_host_tests_scenarios,
    a_broader_run_deferring_only_host_tests_settles_on_their_replays,
    a_broader_run_the_host_cannot_establish_runs_no_replay,
    a_replay_short_of_executing_its_test_leaves_the_broader_run_short,
    a_deferral_owed_to_another_os_holds_the_review_for_it
);
