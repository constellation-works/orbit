// Admitted unit test: combinatorial pure logic over the doctor surface and its
// message shapes — which side of "completed with findings" versus "crashed"
// a message-bearing row lands on.
use super::*;
use crate::contracts::incident::types::DOCTOR_FINDINGS_MESSAGE_PREFIX;

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
