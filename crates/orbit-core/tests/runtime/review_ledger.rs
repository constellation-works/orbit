use chrono::Duration as review_budget_Duration;
use chrono::TimeZone;
use chrono::Utc;
use orbit_store::Store;
use orbit_store::contract::ReviewInvocationRecord;
use orbit_store::contract::ReviewRelease;
use orbit_store::contract::ReviewReserveRequest;
use orbit_types::workflow::ReviewBudget;
use orbit_types::workflow::ReviewReservation;
use orbit_types::workflow::ReviewerInvocationEvent;
use orbit_types::workflow::automation::SourceRevision;

//  A released review attempt's provisional charge is a floor. Later reviewer
//  runtime still counts against the captured minutes, and a start after the
//  budget is spent is bounded to its own start time.

#[test]
fn released_attempt_counts_later_reviewer_runtime_against_the_budget() {
    let store = Store::open_in_memory().expect("memory store");
    let workspace = "ws";
    let lineage = "lineage";
    let meaning = "meaning";
    let tasks = vec!["task-1".to_string()];
    let candidate = SourceRevision {
        commit: "commit".to_string(),
        tree: "tree".to_string(),
    };
    let opened = Utc.with_ymd_and_hms(2026, 10, 4, 12, 0, 0).unwrap();
    let reserve = ReviewReserveRequest {
        lineage_key: lineage,
        task_ids: &tasks,
        run_id: "run-1",
        task_meaning_digest: meaning,
        candidate: &candidate,
        budget: ReviewBudget { minutes: 30 },
        now: opened,
    };
    let (reservation, _) = store
        .write(|tx| orbit_core::test_support::review_ledger::reserve(tx, workspace, &reserve))
        .expect("reserve the candidate's one review");
    let ReviewReservation::Reserved { attempt } = reservation else {
        panic!("a fresh lineage reserves a new attempt");
    };
    let attempt_id = attempt.attempt_id;

    let record = |run_id: &str, event: ReviewerInvocationEvent, now| {
        store
            .write(|tx| {
                orbit_core::test_support::review_ledger::record_invocation(
                    tx,
                    workspace,
                    &ReviewInvocationRecord {
                        lineage_key: lineage,
                        attempt_id: &attempt_id,
                        run_id,
                        event,
                        now,
                    },
                )
            })
            .expect("record reviewer invocation")
    };

    record("run-1", ReviewerInvocationEvent::Started, opened);
    record(
        "run-1",
        ReviewerInvocationEvent::Finished {
            runtime_seconds: 1200,
        },
        opened,
    );
    store
        .write(|tx| {
            orbit_core::test_support::review_ledger::release(
                tx,
                workspace,
                &ReviewRelease {
                    lineage_key: lineage,
                    attempt_id: &attempt_id,
                    bound: opened,
                    now: opened,
                },
            )
        })
        .expect("release the failed run's attempt");

    let released = store
        .read()
        .and_then(|snapshot| snapshot.review().ledger(workspace, lineage))
        .expect("read")
        .expect("ledger");
    let attempt = &released.attempts[0];
    assert_eq!(attempt.elapsed_seconds, Some(1200));
    assert_eq!(attempt.reviewer_seconds, 1200);
    assert_eq!(
        released.remaining_for(&candidate, meaning, opened).seconds,
        600,
        "the provisional release charge leaves the unspent part of the 30-minute budget"
    );

    // The resumed run's reviewer spends the remaining 600s. Finished adds
    // runtime and does not rewrite the provisional charge.
    let resumed_at = opened + review_budget_Duration::minutes(30);
    record(
        "run-1-resume",
        ReviewerInvocationEvent::Finished {
            runtime_seconds: 600,
        },
        resumed_at,
    );

    let spent = store
        .read()
        .and_then(|snapshot| snapshot.review().ledger(workspace, lineage))
        .expect("read")
        .expect("ledger");
    let attempt = &spent.attempts[0];
    assert_eq!(
        attempt.elapsed_seconds,
        Some(1200),
        "release keeps its provisional charge until the next settlement"
    );
    assert_eq!(attempt.reviewer_seconds, 1800);
    assert!(attempt.released_at.is_some());
    assert_eq!(
        spent.remaining_for(&candidate, meaning, resumed_at).seconds,
        0,
        "later reviewer runtime on a released attempt counts against the 30-minute budget"
    );

    let retry_at = resumed_at + review_budget_Duration::minutes(1);
    let started = record("run-1-resume", ReviewerInvocationEvent::Started, retry_at);
    let running = started.attempts[0]
        .reviewer_running
        .as_ref()
        .expect("the start is recorded");
    assert_eq!(running.started_at, retry_at);
    assert_eq!(
        running.deadline, retry_at,
        "with nothing left, the reviewer deadline equals its start time"
    );
}
