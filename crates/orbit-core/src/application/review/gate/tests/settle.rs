//! Settling released attempts, drifted reports, bundle reports and the
//! pre-PR review loop's rework decision.

use std::fs;

use chrono::{Duration, Utc};
use orbit_engine::DispatchError;
use orbit_types::workflow::{FindingDisposition, ReviewAttemptState, ReviewVerdict};
use serde_json::json;

use super::support::{
    BEFORE_PR, gated_bundle_fixture, gated_fixture, report, write_report, write_report_bytes,
};

#[test]
fn a_failed_reviewer_step_leaves_no_open_attempt_and_is_charged_reviewer_runtime_only() {
    let gated = gated_fixture(BEFORE_PR);
    gated.start(&gated.run_id, Utc::now() - Duration::minutes(40));
    let admission = gated.admit().expect("admit");
    let attempt_id = admission["attempt_id"]
        .as_str()
        .expect("attempt")
        .to_string();
    // The reviewer timed out after 15 minutes and its retry failed after 5;
    // backoff, recovery and the run's other 20 minutes ran no reviewer.
    gated.reviewer_ran(&gated.run_id, &admission, 900);
    gated.reviewer_ran(&gated.run_id, &admission, 300);

    gated.release(&gated.run_id, &admission);
    let ledger = gated.ledger(&admission);
    assert!(
        ledger.open_attempt().is_none(),
        "a failed reviewer step never leaves its attempt open"
    );
    let released = &ledger.attempts[0];
    assert_eq!(
        released.state,
        ReviewAttemptState::Settled {
            verdict: ReviewVerdict::Incomplete
        }
    );
    assert!(released.released_at.is_some());
    assert_eq!(
        ledger.consumed_seconds, 1200,
        "exactly the two reviewer invocations are charged"
    );

    let resumed = gated.resume(&gated.run_id);
    gated.reviewer_ran(&resumed, &admission, 60);
    write_report(
        &gated.fixture.runtime,
        &gated.task_id,
        &report(&attempt_id, ReviewVerdict::PassedWithoutRepairs, false),
    );
    let settled = gated
        .settle_in(&resumed, &admission)
        .expect("the resumed run settles the released attempt");
    assert_eq!(settled["gate"], "passed");
    let ledger = gated.ledger(&admission);
    assert_eq!(ledger.attempts.len(), 1, "settling consumed no new start");
    assert_eq!(
        ledger.attempts[0].state,
        ReviewAttemptState::Settled {
            verdict: ReviewVerdict::PassedWithoutRepairs
        }
    );
    assert!(ledger.attempts[0].released_at.is_none());
    assert_eq!(
        ledger.consumed_seconds, 1260,
        "the resumed reviewer adds its own runtime, not the time between runs"
    );

    gated.release(&resumed, &admission);
    assert_eq!(
        gated.ledger(&admission).attempts[0].state,
        ReviewAttemptState::Settled {
            verdict: ReviewVerdict::PassedWithoutRepairs
        },
        "a release never rewrites a settled verdict"
    );
}

#[test]
fn a_drifted_report_on_a_later_bundle_task_still_settles() {
    let gated = gated_bundle_fixture(BEFORE_PR, 2);
    let admission = gated.admit().expect("admit");
    fs::write(
        gated.fixture.repo.join("src.txt"),
        "implementation target\nimplemented\nreviewed\n",
    )
    .expect("reviewer repair");
    let drifted = json!({
        "schema_version": "1",
        "attempt_id": admission["attempt_id"],
        "verdict": "Passed-With-Repairs",
        "summary": "Added the missing note.",
        "findings": [{
            "id": 1,
            "severity": "low",
            "summary": "Missing note",
            "paths": "src.txt",
            "disposition": "repaired",
        }],
        "validation": [{"command": "make ci-fast", "outcome": "PASS"}],
    });
    write_report_bytes(
        &gated.fixture.runtime,
        &gated.bundle[1],
        serde_json::to_vec(&drifted).expect("serialize"),
    );

    let settled = gated
        .settle(&admission)
        .expect("drift that keeps the report's meaning settles");
    assert_eq!(settled["verdict"], "passed_with_repairs");
    let certificate = gated.certificate();
    assert_eq!(certificate.findings.len(), 1);
    assert_eq!(certificate.findings[0].id, "1");
    assert_eq!(
        certificate.findings[0].disposition,
        FindingDisposition::Repaired
    );
}

#[test]
fn the_most_severe_bundle_report_decides_and_refuses_without_retry() {
    let gated = gated_bundle_fixture(BEFORE_PR, 2);
    let admission = gated.admit().expect("admit");
    let attempt_id = admission["attempt_id"].as_str().expect("attempt");
    write_report(
        &gated.fixture.runtime,
        &gated.bundle[0],
        &report(attempt_id, ReviewVerdict::PassedWithoutRepairs, false),
    );
    write_report(
        &gated.fixture.runtime,
        &gated.bundle[1],
        &report(attempt_id, ReviewVerdict::ChangesRequired, false),
    );

    let error = gated
        .settle(&admission)
        .expect_err("changes required blocks");
    assert!(
        matches!(error, DispatchError::DeterministicActionRefused { .. }),
        "a settled verdict is a decision, not a retryable fault: {error}"
    );
    assert!(error.to_string().contains("review_gate_blocked"), "{error}");
    let certificate = gated.certificate();
    assert_eq!(certificate.verdict, ReviewVerdict::ChangesRequired);
    assert!(
        !certificate.rework_requested,
        "only the pre-PR review loop can send findings back for rework"
    );
}

/// [ORB-13891] Inside the pre-PR review loop, `changes_required` with budget
/// left is sent back to the implementer: the settlement hands over the open
/// findings and the head they were raised on, records them on the task, and
/// charges the lineage one repair cycle. Replaying the settlement reproduces
/// it. The reworked head is then admitted as a fresh attempt and its pass is
/// the reviewed head the PR steps publish.
#[test]
fn changes_required_is_sent_back_for_rework_and_the_reworked_head_is_reviewed_again() {
    let gated = gated_fixture(BEFORE_PR);
    let admission = gated.admit().expect("admit");
    let first_attempt = admission["attempt_id"].as_str().expect("attempt");
    write_report(
        &gated.fixture.runtime,
        &gated.task_id,
        &report(first_attempt, ReviewVerdict::ChangesRequired, false),
    );

    let settled = gated
        .settle_in_loop(&admission)
        .expect("an affordable changes_required asks for rework");
    assert_eq!(settled["gate"], "rework_required");
    assert_eq!(settled["verdict"], "changes_required");
    assert_eq!(
        settled["reviewed_head_sha"], "",
        "a rework request is never a reviewed head"
    );
    let rework = &settled["rework"];
    assert_eq!(rework["attempt_id"], first_attempt);
    assert_eq!(rework["head_sha"], gated.implementation_sha.as_str());
    assert_eq!(rework["findings"].as_array().map(Vec::len), Some(1));
    assert_eq!(rework["findings"][0]["id"], "F1");
    assert_eq!(rework["findings"][0]["summary"], "Missing trailing note");

    let ledger = gated.ledger(&admission);
    assert_eq!(
        ledger.attempts[0].state,
        ReviewAttemptState::Settled {
            verdict: ReviewVerdict::ChangesRequired
        }
    );
    assert_eq!(
        ledger.attempts[0].repair_cycles, 1,
        "the granted rework is charged as the lineage's repair cycle"
    );
    assert!(gated.certificate().rework_requested);
    let comments = gated.comments();
    let settlement = comments
        .iter()
        .find(|comment| comment.contains(first_attempt))
        .expect("the settlement is recorded on the task");
    assert!(
        settlement.contains("F1") && settlement.contains("Missing trailing note"),
        "the task records the findings sent back: {settlement}"
    );

    let replayed = gated
        .settle_in_loop(&admission)
        .expect("a replayed settlement reconciles");
    assert_eq!(replayed, settled);
    assert_eq!(gated.ledger(&admission).attempts[0].repair_cycles, 1);

    let reworked = gated.commit_rework("implementation target\nimplemented\nnote\n");
    let readmission = gated.admit().expect("the reworked head is admitted");
    let second_attempt = readmission["attempt_id"].as_str().expect("attempt");
    assert_ne!(second_attempt, first_attempt, "a fresh reviewer start");
    write_report(
        &gated.fixture.runtime,
        &gated.task_id,
        &report(second_attempt, ReviewVerdict::PassedWithoutRepairs, false),
    );
    let passed = gated
        .settle_in_loop(&readmission)
        .expect("the reworked head passes");
    assert_eq!(passed["gate"], "passed");
    assert_eq!(passed["reviewed_head_sha"], reworked.as_str());

    let ledger = gated.ledger(&readmission);
    assert_eq!(ledger.attempts.len(), 2);
    assert_eq!(ledger.consumed().reviewer_starts, 2);
    assert_eq!(ledger.consumed().repair_cycles, 1);
}

/// [ORB-13891] Rework is bounded by the lineage budget. Once no repair
/// cycle or no reviewer start is left to rework and re-review,
/// `changes_required` refuses the settlement and blocks delivery, with every
/// cycle's findings recorded on the task.
#[test]
fn changes_required_without_rework_budget_blocks_with_every_cycles_findings() {
    let gated = gated_fixture(&format!("{BEFORE_PR}review_repair_cycles = 1\n"));
    let admission = gated.admit().expect("admit");
    let first_attempt = admission["attempt_id"].as_str().expect("attempt");
    write_report(
        &gated.fixture.runtime,
        &gated.task_id,
        &report(first_attempt, ReviewVerdict::ChangesRequired, false),
    );
    let settled = gated.settle_in_loop(&admission).expect("first rework");
    assert_eq!(settled["gate"], "rework_required");

    gated.commit_rework("implementation target\nimplemented\nnote\n");
    let readmission = gated.admit().expect("re-admit");
    let second_attempt = readmission["attempt_id"].as_str().expect("attempt");
    let mut second = report(second_attempt, ReviewVerdict::ChangesRequired, false);
    second.findings[0].id = "F2".to_string();
    second.findings[0].summary = "Note is in the wrong place".to_string();
    write_report(&gated.fixture.runtime, &gated.task_id, &second);

    let error = gated
        .settle_in_loop(&readmission)
        .expect_err("no repair cycle is left to rework");
    assert!(
        matches!(error, DispatchError::DeterministicActionRefused { .. }),
        "{error}"
    );
    let message = error.to_string();
    assert!(
        message.contains("review_gate_blocked")
            && message.contains("review_rework_exhausted: review_repair_cycles_exhausted"),
        "{message}"
    );
    let certificate = gated.certificate();
    assert!(!certificate.rework_requested);
    assert_eq!(gated.ledger(&readmission).consumed().repair_cycles, 1);
    let comments = gated.comments().join("\n");
    for (attempt, finding, summary) in [
        (first_attempt, "F1", "Missing trailing note"),
        (second_attempt, "F2", "Note is in the wrong place"),
    ] {
        assert!(
            comments.contains(attempt) && comments.contains(finding) && comments.contains(summary),
            "the task records cycle {attempt}'s findings: {comments}"
        );
    }

    let gated = gated_fixture(&format!("{BEFORE_PR}review_reviewer_starts = 1\n"));
    let admission = gated.admit().expect("admit");
    write_report(
        &gated.fixture.runtime,
        &gated.task_id,
        &report(
            admission["attempt_id"].as_str().expect("attempt"),
            ReviewVerdict::ChangesRequired,
            false,
        ),
    );
    let error = gated
        .settle_in_loop(&admission)
        .expect_err("no reviewer start is left to review a rework");
    assert!(
        error
            .to_string()
            .contains("review_rework_exhausted: review_starts_exhausted"),
        "{error}"
    );
}
