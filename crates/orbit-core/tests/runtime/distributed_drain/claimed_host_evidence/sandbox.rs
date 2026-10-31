//! Verified sandbox refusals at the claimed before-landing settlement boundary.

use super::*;

/// The host replay deliberately reports unavailable infrastructure. These
/// scenarios verify the evidence hold, never count this substitute as coverage.
const UNAVAILABLE_HOST: &str = "echo 'SKIP: sandbox fixture has no host evidence'\n";

fn settle_check(outcome: ValidationOutcome, reason: &str, probe: &str) -> ReviewCertificate {
    let (leaf, attempt_id) = leaf_with_probe_at(probe, true);
    assert_eq!(leaf.gate_input["admission"]["timing"], "before-landing");
    let command = "cargo test -p orbit-exec --test probe";
    reviewer_needs_host(&leaf, &attempt_id, command, host_os());
    // Use the same public report tool as the reviewer, retaining V2 across
    // revisions; a failed assertion cannot escape by naming host evidence.
    let mut report: ReviewReport = serde_json::from_slice(
        &leaf
            .owner_artifact(orbit_types::workflow::REVIEW_REPORT_ARTIFACT)
            .unwrap(),
    )
    .unwrap();
    report.validation[1].outcome = outcome;
    report.validation[1].note = Some(reason.into());
    report.validation[1].sources = vec!["src/f0.rs".into()];
    report.verdict = match outcome {
        ValidationOutcome::Passed => ReviewVerdict::Accept,
        ValidationOutcome::Denied => ReviewVerdict::Incomplete,
        _ => ReviewVerdict::Reject,
    };
    report.escalation = (outcome != ValidationOutcome::Passed).then(|| reason.into());
    if outcome == ValidationOutcome::Passed {
        // The probe ran here successfully; no host replay is owed.
        report.external_evidence.clear();
    }
    leaf.put_report(&report);

    let settlement = leaf.settle();
    let certificate = certificate(&leaf);
    assert_eq!(certificate.validation[1].outcome, outcome);
    assert!(
        certificate.validation[1]
            .note
            .as_deref()
            .unwrap()
            .contains(reason),
        "keep the exact cause in the durable certificate"
    );
    if outcome == ValidationOutcome::Passed {
        let settled = settlement.expect("the successful probe completes the review");
        assert_eq!(settled["gate"], "passed", "{settled}");
        assert_eq!(certificate.verdict, ReviewVerdict::Accept);
        assert!(leaf.owner_artifact(REVIEW_EVIDENCE_HOLD_ARTIFACT).is_none());
    } else if outcome == ValidationOutcome::Denied {
        let refusal = settlement.expect_err("host evidence is unavailable");
        assert!(refusal.contains("review_awaiting_evidence"), "{refusal}");
        assert!(leaf.owner_artifact(REVIEW_EVIDENCE_HOLD_ARTIFACT).is_some());
        assert_eq!(leaf.pair.owner_status(&leaf.task), "in-progress");
    } else {
        let refusal = settlement.expect_err("a real failure blocks the review");
        assert!(refusal.contains("review_gate_blocked"), "{refusal}");
        assert_eq!(certificate.verdict, ReviewVerdict::Reject);
        assert!(leaf.owner_artifact(REVIEW_EVIDENCE_HOLD_ARTIFACT).is_none());
        assert!(
            certificate.host_evidence.is_empty(),
            "a real failure needs repair, not replay"
        );
    }
    certificate
}

fn sandbox_unavailability_holds_but_real_and_unconfirmed_failures_reject() {
    for (outcome, reason) in [
        (
            ValidationOutcome::Denied,
            "sandbox_probe: sandbox_apply: Operation not permitted",
        ),
        (
            ValidationOutcome::Denied,
            "sandbox_probe: verified /usr/bin/git exists; sandbox refused child spawn: Operation not permitted",
        ),
        (
            ValidationOutcome::Failed,
            "src/f0.rs: assertion failed: expected implemented behavior",
        ),
        (
            ValidationOutcome::Failed,
            "git config --get remote.origin.url: No such file or directory; base passed, sandbox cause unconfirmed",
        ),
    ] {
        settle_check(outcome, reason, UNAVAILABLE_HOST);
    }
}

crate::scenario_table!(
    sandbox_unavailability_holds_but_real_and_unconfirmed_failures_reject_scenarios,
    sandbox_unavailability_holds_but_real_and_unconfirmed_failures_reject
);

/// Both native probe outcomes must reach the public settlement boundary.
/// Returning a certificate lets deterministic controls check what was recorded.
fn settle_seatbelt_probe(sandbox_exec: &Path, host_probe: &str) -> ReviewCertificate {
    let output = std::process::Command::new(sandbox_exec)
        .args([
            "-p",
            "(version 1)(allow default)",
            sandbox_exec.to_str().expect("sandbox executable path"),
            "-p",
            "(version 1)(allow default)",
            "/usr/bin/true",
        ])
        .output()
        .expect("spawn sandbox probe");
    if output.status.success() {
        return settle_check(
            ValidationOutcome::Passed,
            "host permits nested Seatbelt profiles; native probe passed",
            host_probe,
        );
    }
    let denial = String::from_utf8_lossy(&output.stderr);
    assert!(
        denial.contains("sandbox_apply: Operation not permitted"),
        "establish the native sandbox cause: {denial}"
    );
    settle_check(ValidationOutcome::Denied, &denial, host_probe)
}

fn stubbed_seatbelt_probe(script: &str) -> ReviewCertificate {
    let sandbox_exec = std::env::current_dir().unwrap().join("sandbox-exec");
    std::fs::write(&sandbox_exec, format!("#!/bin/sh\n{script}\n")).unwrap();
    std::fs::set_permissions(&sandbox_exec, std::fs::Permissions::from_mode(0o755)).unwrap();
    settle_seatbelt_probe(&sandbox_exec, UNAVAILABLE_HOST)
}

fn permitted_seatbelt_probe_records_a_pass_instead_of_vacuous_denial_coverage() {
    let recorded = stubbed_seatbelt_probe("exit 0");
    assert_eq!(recorded.validation[1].outcome, ValidationOutcome::Passed);
    assert_eq!(recorded.verdict, ReviewVerdict::Accept);
}

fn denied_seatbelt_probe_records_the_denial_and_holds_for_host_evidence() {
    let recorded = stubbed_seatbelt_probe(
        "echo 'sandbox-exec: sandbox_apply: Operation not permitted' >&2\nexit 1",
    );
    assert_eq!(recorded.validation[1].outcome, ValidationOutcome::Denied);
    assert_eq!(recorded.verdict, ReviewVerdict::Incomplete);
}

crate::scenario_table!(
    seatbelt_probe_outcomes_scenarios,
    permitted_seatbelt_probe_records_a_pass_instead_of_vacuous_denial_coverage,
    denied_seatbelt_probe_records_the_denial_and_holds_for_host_evidence
);

/// Native macOS evidence: settle a successful nested Seatbelt application as
/// passed, or carry an actual denial through before-landing review and replay.
#[cfg(target_os = "macos")]
#[test]
fn native_seatbelt_probe_records_its_outcome_before_landing() {
    if !crate::admission_audit::isolate(
        module_path!(),
        "native_seatbelt_probe_records_its_outcome_before_landing",
    ) {
        return;
    }
    settle_seatbelt_probe(Path::new("/usr/bin/sandbox-exec"), PROBE);
}
