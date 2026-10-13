//! `orbit run extend` against a pull drain's closing decision [ORB-15417].
//!
//! The job forwards its window answer into the pass, and leaves its loop on
//! the pass's `done`. An extension accepted after that answer, or while the
//! pass is delivering settlements, must keep the drain admitting; one
//! requested after the pass closed the window must be refused rather than
//! reported as accepted on a drain that has already decided to end.

use chrono::{DateTime, Duration, SecondsFormat};
use orbit_core::DrainWindowExtensionRequest;
use orbit_core::application::job::DrainWindowExtensionChange;
use orbit_core::application::job::DrainWindowExtensionOutcome;

use super::*;

/// The pass input the job forwards once its window step answered expired.
fn stale_expired() -> Value {
    json!({"for_seconds": "3600", "window_expired": "true", "max_active_leaf_runs": 1})
}

fn stamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// A running one-hour pull drain whose `open_window` step stamped `deadline`.
fn timed_drain(pair: &Pair, deadline: DateTime<Utc>) -> String {
    let drain =
        pair.run_drain_with_input(json!({"for_seconds": 3600, "destination": pair.destination}));
    stamp_window(&pair.follower, &drain, deadline);
    drain
}

fn stamp_window(follower: &OrbitRuntime, drain: &str, deadline: DateTime<Utc>) {
    let mut state = follower.read_run_state(drain).unwrap().unwrap();
    state.record_pipeline_output("open_window", json!({"deadline": stamp(deadline)}));
    follower.write_run_state(drain, &state).unwrap();
}

fn extend(
    follower: &OrbitRuntime,
    drain: &str,
    concurrency: Option<u32>,
) -> Result<DrainWindowExtensionChange, OrbitError> {
    follower.extend_drain_window(DrainWindowExtensionRequest {
        run_id: drain,
        for_seconds: 3600,
        max_active_leaf_runs: concurrency,
        reason: Some("overnight backlog"),
        actor: "operator",
        source: "fixture",
        claim_token: None,
    })
}

/// The drain's own window answer for the stamp its loop echoes.
fn window(pair: &Pair, drain: &str, echoed: DateTime<Utc>) -> Value {
    pair.follower
        .run_deterministic(
            "drain_window",
            &json!({}),
            &json!({"deadline": stamp(echoed), "run_id": drain}),
            ToolContext::default(),
        )
        .unwrap()
}

fn state(pair: &Pair, drain: &str) -> PipelineState {
    pair.follower.read_run_state(drain).unwrap().unwrap()
}

/// The window answers expired, the operator extends, and the pass the job
/// then runs with that stale answer keeps the drain alive and requests work
/// under the new deadline.
#[test]
fn an_extension_after_the_window_answer_keeps_the_drain_requesting() {
    if !isolated(
        module_path!(),
        "an_extension_after_the_window_answer_keeps_the_drain_requesting",
    ) {
        return;
    }
    let pair = Pair::new(1);
    let lapsed = Utc::now() - Duration::minutes(5);
    let drain = timed_drain(&pair, lapsed);
    assert_eq!(window(&pair, &drain, lapsed)["expired"], true);

    let change = extend(&pair.follower, &drain, None).unwrap();
    assert_eq!(change.outcome, DrainWindowExtensionOutcome::Extended);
    assert_eq!(window(&pair, &drain, lapsed)["expired"], false);

    let pass = pair.pass_over(&drain, stale_expired());
    assert_eq!(pass["done"], false, "{pass}");
    assert!(!pair.wire.calls("orbit.drain.probe").is_empty(), "{pass}");
    assert_eq!(pair.wire.calls("orbit.task.pull").len(), 1, "{pass}");
    assert_eq!(pair.run_state(&drain), JobRunState::Running);
    assert!(state(&pair, &drain).pull_drain.pull_window_close.is_none());
}

/// The extension lands while the pass that read the window closed is
/// delivering the drain's last settlement. The pass rereads the deadline
/// where it decides to end, so the drain keeps running and its next pass
/// requests work.
#[test]
fn an_extension_accepted_during_the_closing_pass_keeps_the_drain_alive() {
    if !isolated(
        module_path!(),
        "an_extension_accepted_during_the_closing_pass_keeps_the_drain_alive",
    ) {
        return;
    }
    let pair = Pair::new(2);
    let drain = timed_drain(&pair, Utc::now() + Duration::minutes(10));
    let leaf = pair.running_leaf(&drain, 1);
    pair.leaf_hands_off(&leaf);
    stamp_window(&pair.follower, &drain, Utc::now() - Duration::minutes(5));

    let operator = pair.follower.clone();
    let operator_drain = drain.clone();
    let accepted = Arc::new(Mutex::new(None));
    let outcome = accepted.clone();
    *pair.wire.before_settle.lock().unwrap() = Some(Box::new(move || {
        let change = extend(&operator, &operator_drain, None).map(|change| change.outcome);
        *outcome.lock().unwrap() = Some(change);
    }));
    let pass = pair.pass_over(&drain, stale_expired());

    let accepted = accepted.lock().unwrap().take().expect("the operator acted");
    assert_eq!(accepted.unwrap(), DrainWindowExtensionOutcome::Extended);
    assert_eq!(
        pass["unsettled"], 0,
        "the last settlement was delivered: {pass}"
    );
    assert_eq!(pass["done"], false, "{pass}");
    assert_eq!(pair.wire.calls("orbit.task.pull").len(), 1, "{pass}");
    assert_eq!(pair.run_state(&drain), JobRunState::Running);
    assert!(state(&pair, &drain).pull_drain.pull_window_close.is_none());

    let next = pair.pass_over(&drain, stale_expired());
    assert_eq!(next["done"], false, "{next}");
    assert_eq!(pair.wire.calls("orbit.task.pull").len(), 2, "{next}");
}

/// With no extension the stale answer ends the drain, and the close is
/// recorded where an extension reads it: a later extension is refused and
/// retunes nothing. A stopped drain whose deadline is still open ends too.
#[test]
fn a_closed_pull_window_ends_the_drain_and_refuses_a_later_extension() {
    if !isolated(
        module_path!(),
        "a_closed_pull_window_ends_the_drain_and_refuses_a_later_extension",
    ) {
        return;
    }
    let pair = Pair::new(1);
    let drain = timed_drain(&pair, Utc::now() - Duration::minutes(5));

    let pass = pair.pass_over(&drain, stale_expired());
    assert_eq!(pass["done"], true, "{pass}");
    assert_eq!(pass["admitting"], false, "{pass}");
    assert!(pair.wire.calls("orbit.drain.probe").is_empty());
    assert!(pair.wire.calls("orbit.task.pull").is_empty());
    assert!(state(&pair, &drain).pull_drain.pull_window_close.is_some());

    let error = extend(&pair.follower, &drain, Some(4))
        .unwrap_err()
        .to_string();
    assert!(error.contains("closed its window"), "{error}");
    let closed = state(&pair, &drain);
    assert!(closed.pull_drain.drain_window_extension.is_none());
    assert!(
        closed.pull_drain.drain_worker_limit.is_none(),
        "a refused extension retunes nothing"
    );

    let open = Utc::now() + Duration::minutes(10);
    let stopped = timed_drain(&pair, open);
    pair.follower
        .stop_workspace_auto_admissions(orbit_core::DrainAdmissionsStopRequest {
            actor: "operator",
            source: "run_auto_stop",
            reason: None,
            claim_token: None,
            force: false,
        })
        .unwrap();
    assert_eq!(window(&pair, &stopped, open)["expired"], true);
    let pass = pair.pass_over(&stopped, stale_expired());
    assert_eq!(pass["done"], true, "{pass}");
    assert!(pair.wire.calls("orbit.task.pull").is_empty());
}
