//! [ORB-15791] A claimed leaf whose provider said its model was at capacity
//! backs that crew off for a bounded time, not for the drain's window: the
//! exclusion carries `until`, the crew runs again in the same window once it
//! passes, and each repeat in the window doubles the backoff up to a cap. A
//! before-landing reviewer held by the backoff names the review's timing and
//! when it lifts.

use chrono::{DateTime, Duration, SubsecRound};

use super::*;

const CAPACITY: &str = "[provider_capacity] cli subprocess exited with code 1: codex provider \
     reported the selected model at capacity: Selected model is at capacity. Please try a \
     different model.";

/// The `until` of the pass's one exclusion of `crew`, which a capacity
/// failure records as `provider_unavailable`.
fn backed_off_until(pass: &Value, crew: &str) -> DateTime<Utc> {
    let exclusions = pass["crews"]["excluded"]
        .as_array()
        .expect("the pass reports its crew window")
        .iter()
        .filter(|exclusion| exclusion["crew"] == crew)
        .collect::<Vec<_>>();
    assert_eq!(exclusions.len(), 1, "{pass}");
    assert_eq!(exclusions[0]["source"], "provider_unavailable", "{pass}");
    serde_json::from_value(exclusions[0]["until"].clone())
        .unwrap_or_else(|_| panic!("a capacity exclusion carries `until`: {pass}"))
}

/// The first capacity failure backs `sol` off for ten minutes from when its
/// leaf ended; once that passed, the same drain claims `sol` work again. The
/// next failure in the window backs it off for twenty, and a drain restricted
/// to `sol` says it admits again once that lifts.
fn a_capacity_failure_backs_the_crew_off_until_a_time_and_it_runs_again_in_the_window() {
    let pair = Pair::with_crews(&[Some("sol"), Some("sol"), Some("sol")]);
    let drain = pair
        .run_drain_with_input(json!({"destination": pair.destination, "allowed_crews": ["sol"]}));
    let first = pair.running_leaf(&drain, 1);
    let first_ended = Utc::now().trunc_subsecs(0) - Duration::minutes(15);
    pair.leaf_fails_at(&first, CAPACITY, first_ended);

    // Its ten-minute backoff passed five minutes ago, so this drain's next
    // pass settles the release and requests `sol` work again.
    let second = pair.running_leaf(&drain, 1);
    assert_ne!(second, first);
    let window = pair
        .follower
        .pull_drain_crew_window(&drain)
        .unwrap()
        .expect("a pull drain has a crew window");
    assert!(
        window
            .excluded
            .iter()
            .all(|exclusion| exclusion.crew != "sol"),
        "{window:#?}"
    );

    let second_ended = Utc::now().trunc_subsecs(0);
    pair.leaf_fails_at(&second, CAPACITY, second_ended);
    let pass = pair.pass(&drain);
    assert_eq!(pass["admitted"], 0, "{pass}");
    assert!(pass["crews"]["host_suppressed"].is_null(), "{pass}");
    assert_eq!(
        backed_off_until(&pass, "sol"),
        second_ended + Duration::minutes(20),
        "the second capacity failure in the window doubles the backoff: {pass}"
    );
    assert!(
        pass["refusal"]
            .as_str()
            .is_some_and(|refusal| refusal.starts_with("no_runnable_crew")
                && refusal.contains("capacity backoff")),
        "a drain held only by the backoff says it admits again once it lifts: {pass}"
    );
    let window = pair
        .follower
        .pull_drain_crew_window(&drain)
        .unwrap()
        .expect("a pull drain has a crew window");
    assert!(window.held_until_lift(), "{window:#?}");
    assert!(
        window
            .describe()
            .iter()
            .any(|line| line.starts_with(&format!(
                "excluded sol (provider_unavailable until {})",
                (second_ended + Duration::minutes(20)).to_rfc3339()
            ))),
        "{window:#?}"
    );
}

/// Each further capacity failure in the window doubles the backoff, up to an
/// hour: the fifth runs to sixty minutes, not 160.
fn repeated_capacity_failures_lengthen_the_backoff_up_to_its_cap() {
    let pair = Pair::with_crews(&[Some("sol"); 5]);
    let drain = pair.run_drain();
    // A pass with its one slot taken requests nothing and succeeds, so the
    // lost bind each leaf's admission stops on never degrades the drain.
    let admit = || {
        let leaf = pair.running_leaf(&drain, 1);
        let full = pair.pass(&drain);
        assert_eq!(full["consecutive_pass_failures"], 0, "{full}");
        leaf
    };
    // Four failures long enough ago that even a capped backoff has passed,
    // so each next leaf is still admitted on `sol`.
    let long_ago = Utc::now().trunc_subsecs(0) - Duration::hours(2);
    for _ in 0..4 {
        let leaf = admit();
        pair.leaf_fails_at(&leaf, CAPACITY, long_ago);
    }
    let fifth = admit();
    let ended = Utc::now().trunc_subsecs(0);
    pair.leaf_fails_at(&fifth, CAPACITY, ended);

    let pass = pair.pass(&drain);
    assert_eq!(
        backed_off_until(&pass, "sol"),
        ended + Duration::minutes(60),
        "{pass}"
    );
}

/// The owner's before-landing reviewer runs as `sol`. While a capacity
/// failure backs `sol` off, the drain requests nothing and its hold names
/// before-landing review and when the backoff lifts.
fn a_backed_off_before_landing_reviewer_holds_requests_until_it_lifts() {
    let pair = Pair::with_owner_config(
        "[review]\nbefore_landing = true\n\n[operation]\nreview_crew = \"sol\"\n",
        &[Some("sol"), Some("sol")],
    );
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    let ended = Utc::now().trunc_subsecs(0);
    pair.leaf_fails_at(&leaf, CAPACITY, ended);
    // This pass probes before it settles the release; the next one probes
    // with `sol` backed off.
    let settled = pair.pass(&drain);
    assert_eq!(settled["admitted"], 0, "{settled}");
    let pulls = pair.wire.calls("orbit.task.pull").len();

    let pass = pair.pass(&drain);

    let until = (ended + Duration::minutes(10)).to_rfc3339();
    let refusal = pass["refusal"].as_str().unwrap_or_default();
    assert!(
        refusal.starts_with(
            "before_pr_reviewer_unavailable: the owner's before-landing review crew `sol` is \
             backed off on the executor until "
        ) && refusal.contains(&until)
            && refusal.contains("Selected model is at capacity"),
        "{pass}"
    );
    assert_eq!(
        pair.wire.calls("orbit.task.pull").len(),
        pulls,
        "the held drain requests nothing: {pass}"
    );
    let recorded = pair
        .follower
        .read_run_state(&drain)
        .unwrap()
        .and_then(|state| state.pull_drain.drain_last_pass)
        .and_then(|last| last.admission_refusal);
    assert_eq!(recorded.as_deref(), Some(refusal), "{pass}");
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    a_capacity_failure_backs_the_crew_off_until_a_time_and_it_runs_again_in_the_window_scenarios,
    a_capacity_failure_backs_the_crew_off_until_a_time_and_it_runs_again_in_the_window,
    repeated_capacity_failures_lengthen_the_backoff_up_to_its_cap,
    a_backed_off_before_landing_reviewer_holds_requests_until_it_lifts
);
