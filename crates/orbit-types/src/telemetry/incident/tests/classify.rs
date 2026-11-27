// Admitted unit test: combinatorial pure logic over the doctor surface and its
// message shapes — which side of "completed with findings" versus "crashed"
// a message-bearing row lands on — and which rows are deferrals, not failures.
use super::super::types::FailureClass;
use crate::telemetry::AuditEventStatus;
use crate::telemetry::incident::classify_failure;
use crate::telemetry::incident::types::{DOCTOR_FINDINGS_MESSAGE_PREFIX, RUN_DEFERRED_SURFACE};

#[test]
fn auto_fix_stops_are_expected_but_genuine_pr_complete_failures_stay_unexpected() {
    let held_stop = "execution failed: deterministic action pr_complete refused: \
                     auto_fix_stopped: pull request #42 cannot be repaired automatically \
                     (required checks have not all concluded)";
    assert_eq!(
        classify_failure(
            "job run-pipeline-worker",
            AuditEventStatus::Failure,
            Some(held_stop),
        ),
        FailureClass::Expected,
        "an auto-fix stop is a documented hold even when its audit row has failure status"
    );

    let verified_live_completion_refusal = "task 'ORB-28367' cannot move to done while linked run \
                                            'jrun-20261125-095834' has a verified-live owner";
    assert_eq!(
        classify_failure(
            "task.mutation",
            AuditEventStatus::Failure,
            Some(verified_live_completion_refusal),
        ),
        FailureClass::Expected,
        "the verified-live completion guard is an expected refusal even when its audit row has failure status"
    );

    let genuine_failure = "execution failed: deterministic action pr_complete failed: \
                          database unavailable";
    assert_eq!(
        classify_failure(
            "job run-pipeline-worker",
            AuditEventStatus::Failure,
            Some(genuine_failure),
        ),
        FailureClass::Unexpected,
        "a pr_complete failure without the hold marker must remain unexpected"
    );
}

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
fn stale_base_refusals_are_expected_but_unrelated_setup_failures_stay_unexpected() {
    let held_diverged = "execution failed: deterministic action worktree_setup refused: \
                         worktree setup refusing stale base: owner checkout '/owner' holds \
                         'agent-main' at a1b2c3, and origin/agent-main is at d4e5f6; the \
                         checkout has diverged from origin, so it was not moved. Left it \
                         untouched for operator inspection. Reconcile the checkout with \
                         origin, then retry.";
    let held_tracked_changes = "execution failed: deterministic action worktree_setup refused: \
                                worktree setup refusing stale base: owner checkout '/owner' \
                                holds 'agent-main' at a1b2c3, and origin/agent-main is at \
                                d4e5f6; the checkout has tracked changes, so the run was not \
                                admitted. Left it untouched for operator inspection. \
                                Reconcile the checkout with origin, then retry.";
    let unattached = "execution failed: deterministic action worktree_setup refused: \
                      worktree setup refusing stale base: no checkout holds 'agent-main', \
                      which is at a1b2c3, and origin/agent-main is at d4e5f6; the branch \
                      has diverged from origin, so it was not moved. Left it untouched for \
                      operator inspection. Reconcile 'agent-main' with origin/agent-main, \
                      then retry.";
    for refusal in [held_diverged, held_tracked_changes, unattached] {
        assert_eq!(
            classify_failure(
                "job run-pipeline-worker",
                AuditEventStatus::Failure,
                Some(refusal),
            ),
            FailureClass::Expected,
            "a stale-base worktree setup refusal is a documented hold even when its audit row has failure status"
        );
    }

    let unrelated_setup_failure = "execution failed: deterministic action worktree_setup failed: \
                                   git fetch origin failed: network unreachable";
    assert_eq!(
        classify_failure(
            "job run-pipeline-worker",
            AuditEventStatus::Failure,
            Some(unrelated_setup_failure),
        ),
        FailureClass::Unexpected,
        "a worktree setup failure without the stale-base marker must stay unexpected"
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
