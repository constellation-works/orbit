//! Settling released attempts, drifted reports, bundle reports, and the
//! three outcomes of a reviewer that fixes its own findings [ORB-13989].

use std::fs;

use chrono::{Duration, Utc};
use orbit_engine::DispatchError;
use orbit_types::workflow::{FindingDisposition, ReviewAttemptState, ReviewFinding, ReviewVerdict};
use serde_json::json;

use super::support::{
    BEFORE_PR, counterfactual, gated_bundle_fixture, gated_fixture, report, write_report,
    write_report_bytes,
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
        &report(&attempt_id, ReviewVerdict::Accept, false),
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
            verdict: ReviewVerdict::Accept
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
            verdict: ReviewVerdict::Accept
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
        "validation": [{"id": 1, "command": "make ci-fast", "outcome": "PASS"}],
    });
    write_report_bytes(
        &gated.fixture.runtime,
        &gated.bundle[1],
        serde_json::to_vec(&drifted).expect("serialize"),
    );

    let settled = gated
        .settle(&admission)
        .expect("drift that keeps the report's meaning settles");
    assert_eq!(
        settled["verdict"], "accept_with_fixes",
        "the pre-ORB-13989 label still reads as the same decision"
    );
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
        &report(attempt_id, ReviewVerdict::Accept, false),
    );
    write_report(
        &gated.fixture.runtime,
        &gated.bundle[1],
        &report(attempt_id, ReviewVerdict::Reject, false),
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
    assert_eq!(certificate.verdict, ReviewVerdict::Reject);
}

/// [ORB-13989] A reviewer with fixable findings fixes them. Settlement
/// records the fixes as one reviewer commit on top of the untouched
/// implementation commit, posts every finding with what changed for it as a
/// task comment, and hands the PR steps the revalidation trigger and the
/// "Review fixes" body section.
#[test]
fn fixable_findings_become_one_reviewer_commit_over_the_untouched_implementation() {
    let gated = gated_fixture(BEFORE_PR);
    let admission = gated.admit().expect("admit");
    let attempt_id = admission["attempt_id"].as_str().expect("attempt");
    gated.reviewer_edits("implementation target\nimplemented\nnote\n");
    write_report(
        &gated.fixture.runtime,
        &gated.task_id,
        &report(attempt_id, ReviewVerdict::AcceptWithFixes, true),
    );

    let settled = gated.settle(&admission).expect("fixed findings accept");
    assert_eq!(settled["gate"], "passed");
    assert_eq!(settled["verdict"], "accept_with_fixes");
    assert_eq!(settled["reviewer_fixed"], true);
    assert_eq!(
        settled["implementation_head_sha"],
        gated.implementation_sha.as_str()
    );

    let head = gated.log("HEAD", "%H");
    assert_eq!(settled["reviewed_head_sha"], head.as_str());
    assert_eq!(
        gated.log("HEAD^", "%H"),
        gated.implementation_sha,
        "the implementation commit is never amended"
    );
    assert!(
        gated.log("HEAD", "%s").starts_with("review: "),
        "the reviewer commit is `review: <summary>`"
    );
    assert!(
        gated.log("HEAD", "%an").contains("reviewer"),
        "the reviewer commit is attributed to the reviewer, not the implementer"
    );
    assert!(
        gated
            .log("HEAD", "%(trailers:key=Orbit-Review-Crew,valueonly)")
            .contains("reviewers"),
        "the reviewer commit names the reviewer crew"
    );

    let certificate = gated.certificate();
    assert_eq!(certificate.repair_commits.len(), 1);
    assert_eq!(certificate.repair_commits[0].commit, head);
    let comment = gated
        .comments()
        .into_iter()
        .find(|comment| comment.contains(attempt_id))
        .expect("the settlement posts its findings on the task");
    for expected in ["F1", "Missing trailing note", "Appended the trailing note"] {
        assert!(comment.contains(expected), "{expected} in {comment}");
    }
    let fixes = settled["review_fixes"].as_str().expect("review fixes");
    assert!(
        fixes.starts_with("## Review fixes"),
        "the PR body section: {fixes}"
    );
    assert!(fixes.contains(&head) && fixes.contains("Appended the trailing note"));
}

/// [ORB-13989] No findings: `accept` adds no reviewer commit, while the PR
/// still carries the raw validation evidence from the certificate.
#[test]
fn no_findings_accepts_without_a_reviewer_commit() {
    let gated = gated_fixture(BEFORE_PR);
    let admission = gated.admit().expect("admit");
    let attempt_id = admission["attempt_id"].as_str().expect("attempt");
    write_report(
        &gated.fixture.runtime,
        &gated.task_id,
        &report(attempt_id, ReviewVerdict::Accept, false),
    );

    let settled = gated.settle(&admission).expect("accept");
    assert_eq!(settled["verdict"], "accept");
    assert_eq!(settled["reviewer_fixed"], false);
    assert!(
        settled["review_fixes"]
            .as_str()
            .unwrap_or_default()
            .contains("## Review validation")
    );
    assert_eq!(
        settled["reviewed_head_sha"],
        gated.implementation_sha.as_str()
    );
    assert_eq!(gated.log("HEAD", "%H"), gated.implementation_sha);
    assert!(gated.certificate().repair_commits.is_empty());
    assert!(
        gated
            .comments()
            .iter()
            .any(|comment| comment.contains(attempt_id)),
        "an accept is recorded on the task too"
    );
}

/// [ORB-13989] A finding the reviewer cannot fix is `reject`: settlement
/// refuses without retry so the failure handoff blocks the task, nothing
/// goes back to an implementer, and the candidate keeps the implementation
/// commit plus the reviewer's commit for whatever it did fix.
#[test]
fn an_unfixable_finding_rejects_and_preserves_both_commits() {
    let gated = gated_fixture(BEFORE_PR);
    let admission = gated.admit().expect("admit");
    let attempt_id = admission["attempt_id"].as_str().expect("attempt");
    gated.reviewer_edits("implementation target\nimplemented\nnote\n");
    let mut rejected = report(attempt_id, ReviewVerdict::Reject, true);
    rejected.findings.push(ReviewFinding {
        id: "F2".to_string(),
        severity: "high".to_string(),
        summary: "The approach contradicts the acceptance criteria".to_string(),
        paths: vec!["src.txt".to_string()],
        disposition: FindingDisposition::Open,
        change: None,
    });
    write_report(&gated.fixture.runtime, &gated.task_id, &rejected);

    let error = gated.settle(&admission).expect_err("reject blocks");
    assert!(
        matches!(error, DispatchError::DeterministicActionRefused { .. }),
        "a reject is a decision, not a retryable fault: {error}"
    );
    assert!(error.to_string().contains("review_gate_blocked"), "{error}");
    let certificate = gated.certificate();
    assert_eq!(certificate.verdict, ReviewVerdict::Reject);
    assert_eq!(certificate.findings.len(), 2);
    assert_eq!(
        gated.log("HEAD^", "%H"),
        gated.implementation_sha,
        "the implementation commit is preserved under the reviewer commit"
    );
    assert_eq!(
        certificate.repair_commits[0].commit,
        gated.log("HEAD", "%H")
    );
    let ledger = gated.ledger(&admission);
    assert_eq!(ledger.attempts.len(), 1, "one review per candidate");
    let comment = gated
        .comments()
        .into_iter()
        .find(|comment| comment.contains(attempt_id))
        .expect("the findings comment");
    for finding in [
        "F1",
        "F2",
        "The approach contradicts the acceptance criteria",
    ] {
        assert!(comment.contains(finding), "{finding} in {comment}");
    }
}

/// [ORB-14616] ORB-14521's accept: a test-only candidate whose reviewer
/// proved the repaired test guards a production file outside the scope by
/// mutating that file and restoring it. Listed as the counterfactual's
/// `mutation_target` and left byte-identical, it settles `accept`; left
/// modified, the review settles `incomplete` naming the file; and the
/// control's checks (its `sources`) must still lie inside the scope.
#[test]
fn a_counterfactual_mutation_of_an_out_of_scope_file_settles_by_its_restoration() {
    struct Case {
        name: &'static str,
        sources: &'static [&'static str],
        left_modified: bool,
        escalation: Option<&'static str>,
    }
    for case in [
        Case {
            name: "restored byte-identical",
            sources: &["src.txt"],
            left_modified: false,
            escalation: None,
        },
        Case {
            name: "left modified in the final candidate",
            sources: &["src.txt"],
            left_modified: true,
            escalation: Some("control `make test-guard` mutated `README.md`"),
        },
        Case {
            name: "checks outside the scope",
            sources: &["README.md"],
            left_modified: false,
            escalation: Some("negative control `make test-guard` names `README.md`, outside"),
        },
    ] {
        let gated = gated_fixture(BEFORE_PR);
        let admission = gated.admit().expect("admit");
        let attempt_id = admission["attempt_id"].as_str().expect("attempt");
        if case.left_modified {
            fs::write(
                gated.fixture.repo.join("README.md"),
                "fixture
mutated
",
            )
            .expect("mutate the guarded file");
        }
        let mut accepted = report(attempt_id, ReviewVerdict::Accept, false);
        accepted
            .validation
            .push(counterfactual(case.sources, &["README.md"]));
        write_report(&gated.fixture.runtime, &gated.task_id, &accepted);

        let settled = gated.settle(&admission);
        let certificate = gated.certificate();
        match case.escalation {
            None => {
                let settled = settled.unwrap_or_else(|error| panic!("{}: {error}", case.name));
                assert_eq!(settled["verdict"], "accept", "{}", case.name);
                assert_eq!(certificate.verdict, ReviewVerdict::Accept, "{}", case.name);
                assert!(certificate.validation_complete, "{}", case.name);
                assert_eq!(gated.log("HEAD", "%H"), gated.implementation_sha);
            }
            Some(expected) => {
                let error = settled.expect_err(case.name);
                assert!(
                    error.to_string().contains("review_gate_blocked"),
                    "{}: {error}",
                    case.name
                );
                assert_eq!(
                    certificate.verdict,
                    ReviewVerdict::Incomplete,
                    "{}",
                    case.name
                );
                let escalation = certificate.escalation.unwrap_or_default();
                assert!(escalation.contains(expected), "{}: {escalation}", case.name);
            }
        }
    }
}

/// A repair outside the admitted selectors widens them and changes the
/// task-meaning digest. Consumption stays the runtime of the attempt that
/// was reserved under the admission digest.
#[test]
fn widening_selectors_reports_the_reviewers_consumed_time() {
    let gated = gated_fixture(BEFORE_PR);
    let admission = gated.admit().expect("admit");
    let attempt_id = admission["attempt_id"]
        .as_str()
        .expect("attempt")
        .to_string();
    let admitted_digest = gated.ledger(&admission).attempts[0]
        .task_meaning_digest
        .clone();
    gated.reviewer_ran(&gated.run_id, &admission, 900);
    fs::write(
        gated.fixture.repo.join("README.md"),
        "fixture\nreviewed outside the admitted selectors\n",
    )
    .expect("reviewer repair outside selectors");
    let mut repaired = report(&attempt_id, ReviewVerdict::AcceptWithFixes, true);
    repaired.findings[0].paths = vec!["README.md".to_string()];
    repaired.findings[0].change = Some("Noted the out-of-scope repair".to_string());
    write_report(&gated.fixture.runtime, &gated.task_id, &repaired);

    let settled = gated
        .settle(&admission)
        .expect("a repair outside the admitted selectors still settles");
    assert_eq!(settled["gate"], "passed");
    assert_eq!(settled["consumed"]["seconds"], 900);

    let certificate = gated.certificate();
    assert_eq!(
        certificate.consumed.seconds, 900,
        "widening selectors must not zero the reviewer's consumed time"
    );
    assert!(
        certificate
            .selectors_widened
            .iter()
            .any(|selector| selector == "file:README.md"),
        "the repair outside the admitted selectors widens them: {:?}",
        certificate.selectors_widened
    );
    assert_ne!(
        certificate.task_meaning_digest, admitted_digest,
        "the certificate keeps the post-widening task-meaning digest"
    );
    assert_eq!(
        gated.ledger(&admission).attempts[0].task_meaning_digest,
        admitted_digest,
        "settlement leaves the attempt on the digest it was admitted under"
    );
}
