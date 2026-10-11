//! [ORB-15630] A claimed leaf's before-landing review settles the owner's
//! required gates on the claim host's own runs, not on its sandboxed
//! reviewer's.
//!
//! The leaf's `validate` step ran the owner's required commands on its host,
//! outside any agent sandbox, on the head it then published. The review gate
//! reads those results from the run, hands them to the reviewer in its
//! manifest, and settles each gate from them: a host pass outweighs a
//! sandboxed failure or skip, and a host failure rejects whatever the
//! reviewer reported. On a macOS host, Linux CodeQL is the owner's and the
//! pull request's hosted CI's, not the review's.

use super::claimed_owed_evidence::{artifact, ci_fast, codeql_rule, owed, record, report};
use super::claimed_review::{REVIEW_CREW, ReviewedLeaf};
use super::*;

use orbit_types::task::HostOs;
use orbit_types::workflow::{
    BaselineRedFailures, EvidenceHostOs, FindingDisposition, KnownRedValidation,
    REVIEW_EVIDENCE_HOLD_ARTIFACT, REVIEW_GATE_ARTIFACT, REVIEW_MANIFEST_ARTIFACT,
    ReviewCertificate, ReviewEvidenceHold, ReviewEvidenceKind, ReviewEvidenceRequirement,
    ReviewFinding, ReviewManifest, ReviewVerdict, ValidationOutcome, ValidationRole,
};

/// The owner's one required command.
const CI_FAST: &str = "make ci-fast";

/// A claimed leaf on a macOS follower, from a before-landing owner that
/// requires [`CI_FAST`] and whose workspace adds `extra`, with the gate
/// input `task_claimed_pr_pipeline` passes its before-landing admission.
fn mac_landing_leaf(extra: &str) -> ReviewedLeaf {
    let mut leaf = ReviewedLeaf::admit_from(
        &format!(
            "[workflow]\nrequired_validation_commands = [\"{CI_FAST}\"]\n\n\
             [review]\nbefore_landing = true\n\n\
             [operation]\nreview_crew = \"{REVIEW_CREW}\"\n{extra}"
        ),
        "",
    );
    leaf.bound = leaf.bound.clone().with_host_os(Some(HostOs::Macos));
    leaf.gate_input["before_landing"] = json!(true);
    leaf.gate_input["host_validation_step"] = json!("validate");
    leaf
}

/// The leaf's `validate` step, as `claim_validate` records it: the host ran
/// [`CI_FAST`] on the published head and it exited `exit_code`, with the
/// known-red verdict `known_red` when the base fails the same tests.
fn host_validated(leaf: &ReviewedLeaf, exit_code: i32, known_red: Option<KnownRedValidation>) {
    let head = git(&leaf.pair.follower_repo, &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    let mut state = leaf
        .pair
        .follower
        .read_run_state(&leaf.leaf)
        .unwrap()
        .unwrap_or_else(|| PipelineState::new(leaf.leaf.clone(), LEAF_JOB.to_string(), json!({})));
    state.pipeline["validate"] = json!({
        "decision": "performed",
        "publication": "pending",
        "commands": if exit_code == 0 { json!([CI_FAST]) } else { json!([]) },
        "known_red": known_red.iter().collect::<Vec<_>>(),
        "results": [{
            "command": CI_FAST,
            "output": format!("{CI_FAST} exited {exit_code}"),
            "exit_code": exit_code,
        }],
        "tested_head": head,
        "validated_base": leaf.base.commit,
        "replayed": false,
    });
    leaf.pair
        .follower
        .write_run_state(&leaf.leaf, &state)
        .unwrap();
}

/// The reviewer's in-sandbox run of the required gate, which a nested
/// sandbox stopped.
fn sandbox_failure() -> orbit_types::workflow::ReviewValidation {
    let mut failed = record("V1", CI_FAST, ValidationOutcome::Failed);
    failed.note = Some("sandbox_apply: Operation not permitted".into());
    failed
}

/// A Mac reviewer whose only blocker is its own sandbox's run of the
/// required gate — failed, or skipped and named as host evidence — settles
/// `accept` on the host's passing run of the same gate.
#[test]
fn a_sandboxed_reviewers_gate_failure_settles_on_the_hosts_pass() {
    if !isolated(
        module_path!(),
        "a_sandboxed_reviewers_gate_failure_settles_on_the_hosts_pass",
    ) {
        return;
    }
    let skipped = {
        let mut skipped = record("V1", CI_FAST, ValidationOutcome::NotRun);
        skipped.note = Some("a nested sandbox cannot apply here".into());
        skipped
    };
    let named = ReviewEvidenceRequirement {
        kind: ReviewEvidenceKind::HostSandboxTest,
        name: "ci-fast outside the sandbox".into(),
        command: CI_FAST.into(),
        artifact: "evidence/ci-fast.json".into(),
        os: Some(EvidenceHostOs::Macos),
    };
    for (verdict, reviewed, evidence) in [
        (ReviewVerdict::Reject, sandbox_failure(), Vec::new()),
        (ReviewVerdict::Incomplete, skipped, vec![named]),
    ] {
        let mut leaf = mac_landing_leaf("");
        host_validated(&leaf, 0, None);
        let admitted = leaf.admit_review();
        assert_eq!(
            admitted["host_validation"][0]["outcome"], "passed",
            "{admitted}"
        );
        let manifest: ReviewManifest = artifact(&leaf, REVIEW_MANIFEST_ARTIFACT).unwrap();
        assert_eq!(
            manifest.host_validation.len(),
            1,
            "the reviewer is told the host decides the gate"
        );

        let mut reviewer_report = report(
            &admitted,
            verdict,
            Vec::new(),
            vec![reviewed],
            Some("make ci-fast does not pass inside the agent sandbox"),
        );
        reviewer_report.external_evidence = evidence;
        leaf.put_report(&reviewer_report);
        let settled = leaf
            .settle()
            .unwrap_or_else(|error| panic!("{verdict:?}: {error}"));
        assert_eq!(settled["verdict"], "accept", "{settled}");

        let certificate: ReviewCertificate = artifact(&leaf, REVIEW_GATE_ARTIFACT).unwrap();
        assert!(certificate.validation_complete);
        assert_eq!(certificate.host_validation, manifest.host_validation);
        let gate = certificate
            .validation
            .iter()
            .find(|record| record.command == CI_FAST)
            .unwrap();
        assert_eq!(
            (gate.role, gate.outcome),
            (ValidationRole::Required, ValidationOutcome::Passed)
        );
        assert!(
            artifact::<ReviewEvidenceHold>(&leaf, REVIEW_EVIDENCE_HOLD_ARTIFACT).is_none(),
            "nothing is held"
        );
    }
}

/// A host run that failed only tests the base fails too is known red: the
/// reviewer's sandboxed failure of the gate settles `accept`, its record
/// still failed and the certificate naming the host's verdict.
#[test]
fn a_known_red_host_gate_settles_the_reviewers_failure_as_known_red() {
    if !isolated(
        module_path!(),
        "a_known_red_host_gate_settles_the_reviewers_failure_as_known_red",
    ) {
        return;
    }
    let mut leaf = mac_landing_leaf("");
    let head = git(&leaf.pair.follower_repo, &["rev-parse", "HEAD"])
        .trim()
        .to_string();
    let known = KnownRedValidation {
        command: CI_FAST.into(),
        base_sha: leaf.base.commit.clone(),
        tested_head: head,
        run_id: leaf.leaf.clone(),
        failures: BaselineRedFailures {
            identities: vec!["test flaky::on_base".into()],
            ..Default::default()
        },
    };
    host_validated(&leaf, 101, Some(known.clone()));
    let admitted = leaf.admit_review();
    leaf.put_report(&report(
        &admitted,
        ReviewVerdict::Reject,
        Vec::new(),
        vec![sandbox_failure()],
        Some("make ci-fast fails"),
    ));
    let settled = leaf.settle().expect("known red continues");
    assert_eq!(settled["verdict"], "accept", "{settled}");
    let certificate: ReviewCertificate = artifact(&leaf, REVIEW_GATE_ARTIFACT).unwrap();
    assert_eq!(
        certificate.known_red.len(),
        1,
        "{:?}",
        certificate.known_red
    );
    assert_eq!(certificate.known_red[0].validation, known);
    let gate = certificate
        .validation
        .iter()
        .find(|record| record.command == CI_FAST)
        .unwrap();
    assert_eq!(gate.outcome, ValidationOutcome::Failed);
}

/// A host run that failed on the candidate for its own reasons is the
/// candidate's failure: the review rejects even though the reviewer passed
/// the gate and accepted.
#[test]
fn a_host_gate_failure_rejects_whatever_the_reviewer_reported() {
    if !isolated(
        module_path!(),
        "a_host_gate_failure_rejects_whatever_the_reviewer_reported",
    ) {
        return;
    }
    let mut leaf = mac_landing_leaf("");
    host_validated(&leaf, 2, None);
    let admitted = leaf.admit_review();
    leaf.put_report(&report(
        &admitted,
        ReviewVerdict::Accept,
        Vec::new(),
        vec![ci_fast()],
        None,
    ));
    let refused = leaf.settle().expect_err("a failed gate never lands");
    assert!(refused.contains("review_gate_blocked"), "{refused}");
    assert!(refused.contains("host_gate_failed"), "{refused}");
    let certificate: ReviewCertificate = artifact(&leaf, REVIEW_GATE_ARTIFACT).unwrap();
    assert_eq!(certificate.verdict, ReviewVerdict::Reject);
    assert!(!certificate.validation_complete);
}

/// A host pass never outweighs an open finding: the review still blocks.
#[test]
fn a_host_gate_pass_never_clears_an_open_finding() {
    if !isolated(
        module_path!(),
        "a_host_gate_pass_never_clears_an_open_finding",
    ) {
        return;
    }
    let mut leaf = mac_landing_leaf("");
    host_validated(&leaf, 0, None);
    let admitted = leaf.admit_review();
    leaf.put_report(&report(
        &admitted,
        ReviewVerdict::Reject,
        vec![ReviewFinding {
            id: "F1".into(),
            severity: "medium".into(),
            summary: "The stub panics".into(),
            paths: vec!["src/f0.rs".into()],
            disposition: FindingDisposition::Open,
            change: None,
        }],
        vec![sandbox_failure()],
        Some("the stub must be decided"),
    ));
    let refused = leaf.settle().expect_err("an open finding blocks");
    assert!(refused.contains("review_gate_blocked"), "{refused}");
}

/// The Mac reviewer's only gap is the Linux CodeQL run a host-evidence rule
/// owes. Before landing it is the owner's and the pull request's hosted CI's:
/// admission hands it over as owner-owed, the review settles `accept`
/// without a hold, and the certificate records what the owner owes.
#[test]
fn a_mac_before_landing_review_records_linux_codeql_as_owner_owed() {
    if !isolated(
        module_path!(),
        "a_mac_before_landing_review_records_linux_codeql_as_owner_owed",
    ) {
        return;
    }
    let mut leaf = mac_landing_leaf(&codeql_rule("**/*.rs"));
    host_validated(&leaf, 0, None);
    let admitted = leaf.admit_review();
    assert_eq!(admitted["owed_evidence"], json!([]), "{admitted}");
    assert_eq!(
        admitted["owner_owed_evidence"],
        json!([owed()]),
        "{admitted}"
    );
    let mut reviewer_report = report(
        &admitted,
        ReviewVerdict::Incomplete,
        Vec::new(),
        vec![
            ci_fast(),
            record("V2", &owed().command, ValidationOutcome::NotRun),
        ],
        Some("Rust CodeQL cannot run on macOS; a Linux run is owed"),
    );
    reviewer_report.external_evidence = vec![owed()];
    leaf.put_report(&reviewer_report);
    let settled = leaf.settle().expect("owner-owed CodeQL holds nothing");
    assert_eq!(settled["verdict"], "accept", "{settled}");

    let certificate: ReviewCertificate = artifact(&leaf, REVIEW_GATE_ARTIFACT).unwrap();
    assert!(certificate.validation_complete);
    assert!(certificate.owed_evidence.is_empty());
    assert_eq!(certificate.owner_owed_evidence, vec![owed()]);
    assert!(
        artifact::<ReviewEvidenceHold>(&leaf, REVIEW_EVIDENCE_HOLD_ARTIFACT).is_none(),
        "nothing is held"
    );
}

/// A CodeQL run the Mac reviewer reports failed is an answer about the
/// candidate, not a gap the owner fills: the review still blocks.
#[test]
fn a_failed_codeql_record_still_blocks_a_mac_before_landing_review() {
    if !isolated(
        module_path!(),
        "a_failed_codeql_record_still_blocks_a_mac_before_landing_review",
    ) {
        return;
    }
    let mut leaf = mac_landing_leaf(&codeql_rule("**/*.rs"));
    host_validated(&leaf, 0, None);
    let admitted = leaf.admit_review();
    leaf.put_report(&report(
        &admitted,
        ReviewVerdict::Reject,
        Vec::new(),
        vec![
            ci_fast(),
            record("V2", &owed().command, ValidationOutcome::Failed),
        ],
        Some("CodeQL reported an alert"),
    ));
    let refused = leaf.settle().expect_err("a failed check blocks");
    assert!(refused.contains("review_gate_blocked"), "{refused}");
}
