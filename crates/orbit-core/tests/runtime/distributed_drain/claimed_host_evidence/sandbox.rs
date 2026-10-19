//! Verified sandbox refusals at the claimed before-landing settlement boundary.

use super::*;

/// The host replay deliberately reports unavailable infrastructure. These
/// scenarios verify the evidence hold, never count this substitute as coverage.
const UNAVAILABLE_HOST: &str = "echo 'SKIP: sandbox fixture has no host evidence'\n";

fn settle_check(outcome: ValidationOutcome, reason: &str, probe: &str) {
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
    report.verdict = if outcome == ValidationOutcome::Denied {
        ReviewVerdict::Incomplete
    } else {
        ReviewVerdict::Reject
    };
    report.escalation = Some(reason.into());
    leaf.put_report(&report);

    let refusal = leaf.settle().expect_err("host evidence is unavailable");
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
    if outcome == ValidationOutcome::Denied {
        assert!(refusal.contains("review_awaiting_evidence"), "{refusal}");
        assert!(leaf.owner_artifact(REVIEW_EVIDENCE_HOLD_ARTIFACT).is_some());
        assert_eq!(leaf.pair.owner_status(&leaf.task), "in-progress");
    } else {
        assert!(refusal.contains("review_gate_blocked"), "{refusal}");
        assert_eq!(certificate.verdict, ReviewVerdict::Reject);
        assert!(leaf.owner_artifact(REVIEW_EVIDENCE_HOLD_ARTIFACT).is_none());
        assert!(
            certificate.host_evidence.is_empty(),
            "a real failure needs repair, not replay"
        );
    }
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

/// Native macOS evidence: attempt nested Seatbelt application, then carry
/// the actual denial through a claimed before-landing review and host replay.
#[cfg(target_os = "macos")]
#[test]
fn native_seatbelt_denial_is_unavailable_before_landing() {
    if !crate::admission_audit::isolate(
        module_path!(),
        "native_seatbelt_denial_is_unavailable_before_landing",
    ) {
        return;
    }
    let output = std::process::Command::new("/usr/bin/sandbox-exec")
        .args([
            "-p",
            "(version 1)(allow default)",
            "/usr/bin/sandbox-exec",
            "-p",
            "(version 1)(allow default)",
            "/usr/bin/true",
        ])
        .output()
        .expect("spawn native sandbox probe");
    let denial = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "Seatbelt cannot nest");
    assert!(
        denial.contains("sandbox_apply: Operation not permitted"),
        "establish the native sandbox cause: {denial}"
    );
    settle_check(ValidationOutcome::Denied, &denial, PROBE);
}
