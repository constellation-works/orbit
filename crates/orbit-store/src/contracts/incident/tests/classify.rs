// Admitted unit test: combinatorial pure logic over the doctor surface and its
// message shapes — which side of "completed with findings" versus "crashed"
// a message-bearing row lands on — and which rows are deferrals, not failures.
use super::*;
use crate::contracts::incident::types::{DOCTOR_FINDINGS_MESSAGE_PREFIX, RUN_DEFERRED_SURFACE};

#[test]
fn doctor_findings_are_expected_but_a_doctor_crash_stays_unexpected() {
    let findings = format!("{DOCTOR_FINDINGS_MESSAGE_PREFIX}1 failure (review), 3 warnings");
    assert_eq!(
        classify_failure("doctor", AuditEventStatus::Failure, Some(&findings)),
        FailureClass::Expected,
        "a doctor that ran its checks and reported findings is a verdict, not a fault"
    );
    for crash in [
        Some("io error: broken pipe reading config"),
        Some("sqlite: database disk image is malformed"),
        None,
    ] {
        assert_eq!(
            classify_failure("doctor", AuditEventStatus::Failure, crash),
            FailureClass::Unexpected,
            "a doctor that errors before completing its checks must stay unexpected: {crash:?}"
        );
    }
    assert_eq!(
        classify_failure("task show", AuditEventStatus::Failure, Some(&findings)),
        FailureClass::Unexpected,
        "the findings prefix only exempts the doctor surface"
    );
}

#[test]
fn deferral_rows_and_provider_usage_limits_classify_as_deferred() {
    let limit = "step `implement` failed: [provider_limit] provider=codex \
                 resets_at=2026-10-09T22:42:00Z You've hit your usage limit.";
    for (surface, message) in [
        (
            RUN_DEFERRED_SURFACE,
            Some("ci_failure_sweep deferred current failures"),
        ),
        (RUN_DEFERRED_SURFACE, None),
        ("pipeline.worker.exit", Some(limit)),
        ("run-pipeline-worker", Some(limit)),
    ] {
        assert_eq!(
            classify_failure(surface, AuditEventStatus::Failure, message),
            FailureClass::Deferred,
            "a deferral is listed apart from failures: {surface} {message:?}"
        );
    }
    assert_eq!(
        classify_failure(
            "run-pipeline-worker",
            AuditEventStatus::Failure,
            Some("[provider_capacity] provider=codex cli subprocess exited with code 1"),
        ),
        FailureClass::Unexpected,
        "a provider failure other than a usage limit stays a failure"
    );
}
