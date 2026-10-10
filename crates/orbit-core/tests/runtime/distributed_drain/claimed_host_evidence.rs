//! A claimed leaf's host runs the sandbox-gated test its reviewer could not
//! [ORB-14478].
//!
//! The reviewer runs inside the agent sandbox, where a nested sandbox cannot
//! apply, so it reports `incomplete` and names the test as a
//! `host_sandbox_test` for this host's OS. Settlement on the follower runs
//! it outside the agent sandbox at the final candidate. The follower's
//! validation PATH leads with a stand-in `cargo` that runs the candidate's
//! own probe script, so what the run proves is whatever the candidate's
//! probe does; on macOS the probe applies a real Seatbelt profile.

use std::os::unix::fs::PermissionsExt;

use super::claimed_review::ReviewedLeaf;
use super::*;

use orbit_types::workflow::handoff::HandoffReviewEvidence;
use orbit_types::workflow::{
    EvidenceHostOs, HostEvidenceReason, REVIEW_CONTRACT_VERSION, REVIEW_EVIDENCE_HOLD_ARTIFACT,
    REVIEW_GATE_ARTIFACT, ReviewCertificate, ReviewEvidenceKind, ReviewEvidenceRequirement,
    ReviewExternalEvidence, ReviewReport, ReviewValidation, ReviewVerdict, ValidationOutcome,
    ValidationRole,
};

const ARTIFACT: &str = "evidence/host-sandbox-probe.json";
const LOG_ARTIFACT: &str = "evidence/host-sandbox-probe.log.json";

/// A claimed leaf whose follower resolves `cargo` to a stand-in running
/// `tests/host/<target>.sh` from the checkout it is started in, with the
/// candidate's probe committed as `probe`, and its review admitted under the
/// returned attempt.
fn leaf_with_probe(probe: &str) -> (ReviewedLeaf, String) {
    let home = PathBuf::from(std::env::var("HOME").expect("the isolated child's HOME"));
    let bin = home.join("host-evidence-bin");
    std::fs::create_dir_all(&bin).unwrap();
    let cargo = bin.join("cargo");
    // `cargo test -p <package> --test <target> [filter] -- --nocapture`
    std::fs::write(&cargo, "#!/bin/sh\nexec sh \"tests/host/$5.sh\"\n").unwrap();
    std::fs::set_permissions(&cargo, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut leaf = ReviewedLeaf::admit_with_follower_config(&format!(
        "[workflow.validation_env]\nlogin_shell = false\npath = [\"{}\"]\n",
        bin.display()
    ));
    let repo = leaf.pair.follower_repo.clone();
    std::fs::create_dir_all(repo.join("tests/host")).unwrap();
    std::fs::write(repo.join("tests/host/probe.sh"), probe).unwrap();
    git(&repo, &["add", "tests"]);
    git(&repo, &["commit", "-q", "-m", "Add the sandbox probe"]);
    let admitted = leaf.admit_review();
    let attempt_id = admitted["attempt_id"].as_str().unwrap().to_string();
    (leaf, attempt_id)
}

/// The reviewer checked everything else and names `command` as the test its
/// sandbox could not run, for `os`.
fn reviewer_needs_host(leaf: &ReviewedLeaf, attempt_id: &str, command: &str, os: EvidenceHostOs) {
    leaf.put_report(&ReviewReport {
        external_evidence: vec![ReviewEvidenceRequirement {
            kind: ReviewEvidenceKind::HostSandboxTest,
            name: "Sandbox probe".into(),
            command: command.into(),
            artifact: ARTIFACT.into(),
            os: Some(os),
        }],
        schema_version: REVIEW_CONTRACT_VERSION,
        attempt_id: attempt_id.into(),
        verdict: ReviewVerdict::Incomplete,
        summary: "Checked the change; the sandbox probe needs the host.".into(),
        findings: Vec::new(),
        validation: [
            ("V1", "make ci-fast", ValidationOutcome::Passed),
            ("V2", command, ValidationOutcome::NotRun),
        ]
        .into_iter()
        .map(|(id, command, outcome)| ReviewValidation {
            id: Some(id.into()),
            command: command.into(),
            outcome,
            role: ValidationRole::Required,
            note: Some("sandbox_apply: Operation not permitted".into()),
            check: None,
            control: None,
            sources: Vec::new(),
            mutation_target: Vec::new(),
            deferred: Vec::new(),
            baseline: None,
        })
        .collect(),
        retired_validation: Vec::new(),
        escalation: Some("the sandbox probe needs the host".into()),
    });
}

fn host_os() -> EvidenceHostOs {
    EvidenceHostOs::current().expect("a supported host OS")
}

fn certificate(leaf: &ReviewedLeaf) -> ReviewCertificate {
    serde_json::from_slice(
        &leaf
            .owner_artifact(REVIEW_GATE_ARTIFACT)
            .expect("the certificate is on the owner's task"),
    )
    .unwrap()
}

/// The probe the candidate commits: on macOS it applies a Seatbelt profile
/// as Orbit's sandbox tests do, which fails with `sandbox_apply` EPERM
/// inside another sandbox; elsewhere it records that it ran.
#[cfg(target_os = "macos")]
const PROBE: &str = "set -e\n\
    /usr/bin/sandbox-exec -p '(version 1)(allow default)' /usr/bin/true\n\
    echo \"ORBIT_HOST_PROBE_RAN $(git rev-parse HEAD)\"\n\
    echo 'test probe ... ok'\n\
    echo 'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out'\n";
#[cfg(not(target_os = "macos"))]
const PROBE: &str = "set -e\n\
    echo \"ORBIT_HOST_PROBE_RAN $(git rev-parse HEAD)\"\n\
    echo 'test probe ... ok'\n\
    echo 'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out'\n";

/// The follower runs the reviewer's `host_sandbox_test` outside the agent
/// sandbox at the final candidate, attaches its result and log bound to that
/// tree and command, and the review passes without a hold. The handoff pins
/// both, and the owner accepts it only with them. On macOS the probe applies
/// a real Seatbelt profile, so a pass shows no `sandbox_apply` EPERM.
#[test]
fn a_claimed_hosts_sandbox_test_run_lets_the_review_pass_and_the_owner_accept() {
    if !orbit_exec::macos_sandbox_test_guard(
        "a_claimed_hosts_sandbox_test_run_lets_the_review_pass_and_the_owner_accept",
    ) {
        return;
    }
    if !isolated(
        module_path!(),
        "a_claimed_hosts_sandbox_test_run_lets_the_review_pass_and_the_owner_accept",
    ) {
        return;
    }
    let command = "cargo test -p orbit-exec --test probe";
    let (leaf, attempt_id) = leaf_with_probe(PROBE);
    reviewer_needs_host(&leaf, &attempt_id, command, host_os());

    let settled = leaf.settle().expect("the host's run completes the review");
    assert_eq!(settled["gate"], "passed", "{settled}");
    assert!(
        leaf.owner_artifact(REVIEW_EVIDENCE_HOLD_ARTIFACT).is_none(),
        "no hold"
    );
    let head = git(&leaf.pair.follower_repo, &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    let tree = git(&leaf.pair.follower_repo, &["rev-parse", "HEAD^{tree}"])
        .trim()
        .to_string();

    let certificate = certificate(&leaf);
    assert_eq!(certificate.attempt_id, attempt_id);
    assert!(certificate.verdict.passed(), "{:?}", certificate.verdict);
    let [record] = certificate.host_evidence.as_slice() else {
        panic!("one host run: {:?}", certificate.host_evidence);
    };
    assert!(record.passed, "{record:?}");
    assert_eq!(record.tree, tree);
    assert_eq!(record.command, command);
    assert_eq!(record.os, host_os());

    let result: ReviewExternalEvidence =
        serde_json::from_slice(&leaf.owner_artifact(ARTIFACT).expect("the result")).unwrap();
    assert_eq!(result.kind, ReviewEvidenceKind::HostSandboxTest);
    assert_eq!(result.outcome, ValidationOutcome::Passed);
    assert_eq!(result.command, command);
    assert_eq!(result.candidate.tree, tree);
    assert_eq!(result.os, Some(host_os()));
    let log: Value =
        serde_json::from_slice(&leaf.owner_artifact(LOG_ARTIFACT).expect("the log")).unwrap();
    let output = log["output"].as_str().unwrap_or_default();
    assert!(
        output
            .lines()
            .any(|line| line == format!("ORBIT_HOST_PROBE_RAN {head}")),
        "the host ran the probe at the final candidate: {log:#}"
    );
    assert!(
        !output.contains("sandbox_apply"),
        "the probe met no nested sandbox: {log:#}"
    );

    let evidence: HandoffReviewEvidence =
        serde_json::from_value(settled["handoff_evidence"].clone()).expect("handoff evidence");
    assert_eq!(
        evidence
            .host_evidence
            .iter()
            .map(|pinned| pinned.path.as_str())
            .collect::<Vec<_>>(),
        [ARTIFACT, LOG_ARTIFACT]
    );
    let head = SourceRevision { commit: head, tree };
    let mut unpinned = evidence.clone();
    unpinned.host_evidence.clear();
    let refused = leaf
        .owner_accepts(unpinned, &head)
        .expect_err("the owner verifies the host's run itself")
        .to_string();
    assert!(refused.contains("host"), "{refused}");
    assert_eq!(leaf.pair.owner_status(&leaf.task), "in-progress");
    leaf.owner_accepts(evidence, &head)
        .expect("the owner accepts the handoff with the host's run");
    assert_eq!(leaf.pair.owner_status(&leaf.task), "review");
}

/// A probe that skips itself is missing evidence, never a pass: the result
/// is not attached, the log names the skip, and the review holds.
#[test]
fn a_self_skipped_host_run_holds_the_review() {
    if !isolated(module_path!(), "a_self_skipped_host_run_holds_the_review") {
        return;
    }
    let (leaf, attempt_id) = leaf_with_probe(
        "echo 'SKIP: the probe needs a kernel feature'\n\
         echo 'test probe ... ok'\n\
         echo 'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out'\n",
    );
    reviewer_needs_host(
        &leaf,
        &attempt_id,
        "cargo test -p orbit-exec --test probe",
        host_os(),
    );

    let held = leaf.settle().expect_err("a skipped run is not evidence");
    assert!(held.contains("evidence"), "{held}");
    assert!(
        leaf.owner_artifact(REVIEW_EVIDENCE_HOLD_ARTIFACT).is_some(),
        "the review holds for the evidence"
    );
    assert!(leaf.owner_artifact(ARTIFACT).is_none(), "no result");
    let log: Value =
        serde_json::from_slice(&leaf.owner_artifact(LOG_ARTIFACT).expect("the log")).unwrap();
    assert_eq!(log["reason"], "self_skipped", "{log:#}");
    let certificate = certificate(&leaf);
    assert!(!certificate.verdict.passed());
    let [record] = certificate.host_evidence.as_slice() else {
        panic!("one host run: {:?}", certificate.host_evidence);
    };
    assert!(!record.passed);
    assert_eq!(record.reason, Some(HostEvidenceReason::SelfSkipped));
}

/// A command outside the closed grammar, or carrying shell syntax, is
/// refused with its typed reason and never reaches a shell; the review holds.
#[test]
fn a_host_sandbox_test_outside_the_grammar_is_refused_and_never_run() {
    if !isolated(
        module_path!(),
        "a_host_sandbox_test_outside_the_grammar_is_refused_and_never_run",
    ) {
        return;
    }
    for (command, reason) in [
        (
            "cargo test -p orbit-exec --test probe; touch ran-outside-grammar",
            HostEvidenceReason::ShellMetacharacter,
        ),
        (
            "sh tests/host/probe.sh",
            HostEvidenceReason::CommandNotAllowed,
        ),
    ] {
        let (leaf, attempt_id) = leaf_with_probe("touch ran-outside-grammar\n");
        reviewer_needs_host(&leaf, &attempt_id, command, host_os());
        let held = leaf
            .settle()
            .expect_err("a refused command is not evidence");
        assert!(held.contains("evidence"), "{command}: {held}");
        assert!(leaf.owner_artifact(LOG_ARTIFACT).is_none(), "{command}");
        let certificate = certificate(&leaf);
        let [record] = certificate.host_evidence.as_slice() else {
            panic!("{command}: {:?}", certificate.host_evidence);
        };
        assert_eq!(record.reason, Some(reason), "{command}");
        assert!(record.host_command.is_none(), "{command}");
        assert!(
            certificate
                .escalation
                .as_deref()
                .is_some_and(|escalation| escalation
                    .contains(&format!("host_sandbox_test_{}", reason.as_str()))),
            "{command}: {:?}",
            certificate.escalation
        );
        let repo = &leaf.pair.follower_repo;
        assert!(!repo.join("ran-outside-grammar").exists(), "{command}");
        let worktrees = repo.join(".git/orbit-host-evidence");
        assert!(
            !worktrees.exists() || std::fs::read_dir(&worktrees).unwrap().next().is_none(),
            "{command}: nothing was checked out to run"
        );
    }
}
