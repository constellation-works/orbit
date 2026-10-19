//! The owner's landing of an accepted handoff across provider transport
//! failures, through the real provider adapter and its failure classifier.
//!
//! A transport failure (a TLS handshake timeout, a reset connection) before
//! any external write retries the same handoff with bounded backoff and lands
//! once the provider recovers. A run that still fails, on that or on a
//! permanent refusal, records a stop with its error instead of leaving the
//! attempt dispatched. A failed merge request is never resent: its intent
//! stays unresolved, and the next attempt reconciles it first.
use super::absent_context::{AcceptedLanding, NEW, accepted_landing, setup};
use super::*;

use orbit_engine::HandoffLandingStep;

const TLS: &str = "Post \"https://api.github.com/graphql\": net/http: TLS handshake timeout";
const RESET: &str = "Get \"https://api.github.com/repos/orbit/test/pulls/42\": read tcp \
                     10.0.0.2:51234->140.82.112.6:443: read: connection reset by peer";
const BAD_CREDENTIALS: &str = "gh: Bad credentials (HTTP 401)";
/// Only the PR status read carries this field list.
const STATUS_READ: &str = "--json number,state,isDraft";
const MERGE_WRITE: &str = "pulls/42/merge";
const INTENT: &str = "accepted-fixture:42";

/// One landing run with a check budget, so status reads carry a deadline and
/// get no in-process retry from the provider adapter. The budget fits both
/// backoffs, which a retry must start within. Returns the run's outcome and
/// the backoff waits it asked for, in seconds.
fn land(host: &AcceptedLanding) -> (Result<Value, OrbitError>, Vec<u64>) {
    let waits = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let captured = waits.clone();
    let outcome = orbit_engine::test_support::with_vcs_retry_wait(
        move |wait| captured.borrow_mut().push(wait.as_secs()),
        || {
            execute_deterministic_action(
                host,
                "handoff_land",
                &json!({}),
                &json!({"handoff_id":"accepted-fixture","run_id":RUN_ID,"max_wait_seconds":120}),
                false,
                &HashMap::new(),
                None,
            )
        },
    );
    let waits = waits.borrow().clone();
    (outcome, waits)
}

fn steps(host: &AcceptedLanding) -> Vec<HandoffLandingStep> {
    host.updates
        .lock()
        .unwrap()
        .iter()
        .map(|update| update.step.clone())
        .collect()
}

fn landed_steps() -> Vec<HandoffLandingStep> {
    vec![
        HandoffLandingStep::PublishIntent {
            intent_id: INTENT.into(),
        },
        HandoffLandingStep::ResolveIntent {
            intent_id: INTENT.into(),
            merged: true,
        },
        HandoffLandingStep::Complete,
    ]
}

fn calls(fx: &Fixture, needle: &str) -> usize {
    fs::read_to_string(fx.forge.join("calls"))
        .unwrap_or_default()
        .lines()
        .filter(|call| call.contains(needle))
        .count()
}

fn mutations(fx: &Fixture) -> String {
    fs::read_to_string(fx.forge.join("mutations")).unwrap_or_default()
}

/// A checked, mergeable pull request whose `matched` provider call fails
/// with `stderr`, once or (with `repeat`) every time.
fn faulty_landing(
    row: &Path,
    matched: &str,
    stderr: &str,
    repeat: bool,
) -> (Fixture, AcceptedLanding) {
    let (fx, host) = setup(row, NEW);
    let host = accepted_landing(&fx, host);
    fs::write(fx.forge.join("base-applied"), "current").unwrap();
    fs::write(fx.forge.join("fault-match"), matched).unwrap();
    fs::write(fx.forge.join("fault-stderr"), stderr).unwrap();
    if repeat {
        fs::write(fx.forge.join("fault-repeat"), "repeat").unwrap();
    }
    (fx, host)
}

#[test]
fn owner_landing_retries_transport_failures_and_never_leaves_a_failed_run_dispatched() {
    isolated(
        "landing_transport::owner_landing_retries_transport_failures_and_never_leaves_a_failed_run_dispatched",
        |sandbox| {
            // One transport failure on the status read before any write: the
            // same handoff is read again after one backoff and lands.
            for stderr in [TLS, RESET] {
                in_fresh_row(sandbox, |row| {
                    let (fx, host) = faulty_landing(row, STATUS_READ, stderr, false);

                    let (outcome, waits) = land(&host);

                    let landed = outcome.unwrap_or_else(|error| panic!("{stderr}: {error}"));
                    assert_eq!(landed["phase"], "landed", "{stderr}");
                    assert!(fx.forge.join("fault-hit").exists(), "{stderr}");
                    assert_eq!(waits, [20], "{stderr}: one bounded backoff");
                    assert_eq!(mutations(&fx), "merge\n", "{stderr}: one merge");
                    assert_eq!(steps(&host), landed_steps(), "{stderr}: no stop recorded");
                });
            }

            // A provider that stays down: a bounded number of reads, then a
            // durable stop naming the error, and no merge.
            in_fresh_row(sandbox, |row| {
                let (fx, host) = faulty_landing(row, STATUS_READ, TLS, true);

                let (outcome, waits) = land(&host);

                let error = outcome.expect_err("an unrecovered provider stops the landing");
                assert_eq!(calls(&fx, STATUS_READ), 3, "three passes, no more");
                assert_eq!(waits, [20, 60]);
                assert!(mutations(&fx).is_empty());
                assert_eq!(
                    steps(&host),
                    [HandoffLandingStep::Stop { repairable: false }]
                );
                let evidence = host.updates.lock().unwrap()[0].evidence.clone();
                assert!(
                    evidence.contains("after 3 tries")
                        && evidence.contains("TLS handshake timeout"),
                    "the stop names the provider error: {evidence}"
                );
                assert!(
                    error.to_string().contains("TLS handshake timeout"),
                    "{error}"
                );
            });

            // A permanent refusal is not retried, and still settles the attempt.
            in_fresh_row(sandbox, |row| {
                let (fx, host) = faulty_landing(row, STATUS_READ, BAD_CREDENTIALS, true);

                let (outcome, waits) = land(&host);

                outcome.expect_err("a credential refusal stops the landing");
                assert_eq!(
                    calls(&fx, STATUS_READ),
                    1,
                    "no retry of a permanent refusal"
                );
                assert!(waits.is_empty());
                assert_eq!(
                    steps(&host),
                    [HandoffLandingStep::Stop { repairable: false }]
                );
                let evidence = host.updates.lock().unwrap()[0].evidence.clone();
                assert!(evidence.contains("Bad credentials"), "{evidence}");
            });

            // A transport failure on the merge request itself may have been
            // applied: the run neither resends it nor stops past the intent.
            // The next attempt reconciles the intent before it merges.
            in_fresh_row(sandbox, |row| {
                let (fx, mut host) = faulty_landing(row, MERGE_WRITE, TLS, false);

                let (outcome, waits) = land(&host);

                outcome.expect_err("an unconfirmed merge fails the run");
                assert_eq!(calls(&fx, MERGE_WRITE), 1, "the merge is sent once");
                assert!(waits.is_empty(), "a write is never retried blindly");
                assert_eq!(
                    steps(&host),
                    [HandoffLandingStep::PublishIntent {
                        intent_id: INTENT.into()
                    }],
                    "the intent stays unresolved for reconciliation"
                );

                host.updates.lock().unwrap().clear();
                host.context.unresolved_merge_intent = Some(INTENT.into());
                let (outcome, _) = land(&host);

                assert_eq!(outcome.expect("the provider recovered")["phase"], "landed");
                let mut reconciled = vec![HandoffLandingStep::ResolveIntent {
                    intent_id: INTENT.into(),
                    merged: false,
                }];
                reconciled.extend(landed_steps());
                assert_eq!(steps(&host), reconciled, "reconciled before merging again");
                assert_eq!(mutations(&fx), "merge\n");
            });
        },
    );
}
