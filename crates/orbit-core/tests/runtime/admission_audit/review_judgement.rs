//! Judging the reviewer's claims against the lineage's review budget.

use chrono::{Duration, Utc};
use orbit_automation::review::{combined_task_meaning_digest, task_meaning_digest};
use orbit_engine::DispatchError;
use orbit_store::contracts::ReviewReserveRequest;
use orbit_types::workflow::automation::SourceRevision;
use orbit_types::workflow::{ReviewBudget, ReviewReservation, ReviewVerdict};

use super::review_support::lineage_key;

use super::review_support::{BEFORE_PR, gated_fixture, git, report, write_report};

#[test]
fn the_minutes_limit_refuses_new_starts_but_not_an_admitted_reviewer() {
    if !crate::admission_audit::isolate(
        module_path!(),
        "the_minutes_limit_refuses_new_starts_but_not_an_admitted_reviewer",
    ) {
        return;
    }

    let gated = gated_fixture(&format!("{BEFORE_PR}minutes = 1\n"));
    let task = gated
        .fixture
        .runtime
        .get_task(&gated.task_id)
        .expect("task");
    let digest = task_meaning_digest(&task).expect("digest");
    let combined =
        combined_task_meaning_digest(&[(task.id.to_string(), digest)]).expect("combined");
    let workspace_id = gated.fixture.runtime.workspace_id().expect("workspace");
    let lineage = lineage_key(
        &workspace_id,
        std::slice::from_ref(&gated.task_id),
        "main",
        &gated.run_id,
    );
    let started = Utc::now() - Duration::minutes(2);
    let candidate = SourceRevision {
        commit: gated.implementation_sha.clone(),
        tree: git(&gated.fixture.repo, &["rev-parse", "HEAD^{tree}"]),
    };
    let store = gated.fixture.runtime.review_store().expect("store");
    let ReviewReservation::Reserved { attempt } = store
        .review_reserve(
            &workspace_id,
            &ReviewReserveRequest {
                lineage_key: &lineage,
                task_ids: std::slice::from_ref(&gated.task_id),
                run_id: &gated.run_id,
                task_meaning_digest: &combined,
                candidate: &candidate,
                budget: ReviewBudget { minutes: 1 },
                now: started,
            },
        )
        .expect("pre-reserve")
        .0
    else {
        panic!("pre-reserve a start two minutes ago");
    };

    let admission = gated.admit().expect("resume the overrunning attempt");
    assert_eq!(admission["decision"], "resumed");
    assert_eq!(admission["attempt_id"], attempt.attempt_id);
    gated.reviewer_ran(&gated.run_id, &admission, 120);
    write_report(
        &gated.fixture.runtime,
        &gated.task_id,
        &report(&attempt.attempt_id, ReviewVerdict::Accept, false),
    );
    let settled = gated
        .settle(&admission)
        .expect("an admitted reviewer settles on its evidence, not on the leftover minutes");
    assert_eq!(settled["gate"], "passed");
    let certificate = gated.certificate();
    assert_eq!(certificate.verdict, ReviewVerdict::Accept);
    assert_eq!(certificate.consumed.seconds, 120);

    let error = gated.admit().expect_err("the candidate had its one review");
    assert!(
        matches!(error, DispatchError::DeterministicActionRefused { .. }),
        "a spent review is a decision a retry would only repeat: {error}"
    );
    assert!(
        error.to_string().contains("review_candidate_reviewed"),
        "{error}"
    );
}
