//! [ORB-14257] A launched claimed leaf's settlement carries a typed failure
//! class, and the owner blocks the task only for the candidate's own failure
//! or the task's; every other class releases it, within a per-task budget.

use orbit_types::workflow::{ClaimFailureClass, CrewExclusion, CrewExclusionSource};

use super::*;

/// The settlement the follower delivered for `leaf`'s claim.
fn settlement_of(pair: &Pair, leaf: &str) -> Value {
    let claim_id = pair
        .admission(leaf)
        .receipt
        .and_then(|receipt| receipt.claim)
        .expect("claim")
        .claim_id;
    pair.wire
        .calls("orbit.drain.claim.settle")
        .into_iter()
        .rev()
        .find(|settle| settle["claim_id"] == claim_id.as_str())
        .map(|settle| settle["settlement"].clone())
        .expect("the leaf's settlement was delivered")
}

/// A pass that only settles: its window is over, so it requests no claim.
fn settle_only(pair: &Pair, drain: &str) -> Value {
    pair.pass_over(drain, json!({"window_expired": true}))
}

/// A claimed `sol` leaf that fails on `error`, settled by one pass: the
/// owner's task returns to the backlog, and the release names `class`.
/// Returns that settle-only pass and the drain, for the caller's window
/// assertions.
fn a_released_failure(error: &str, class: &str) -> (Pair, String, Value) {
    let pair = Pair::with_crews(&[Some("sol")]);
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    let task = pair.claimed_task(&leaf);
    pair.leaf_fails_with(&leaf, error);

    let pass = settle_only(&pair, &drain);
    assert_eq!(pair.owner_status(&task), "backlog", "{pass}");
    assert_eq!(
        pass["consecutive_failures"], 0,
        "a release is no failure: {pass}"
    );
    let release = &settlement_of(&pair, &leaf)["Release"];
    assert_eq!(release["failure"]["class"], class, "{release}");
    assert_eq!(release["failure"]["crew"], "sol", "{release}");
    assert!(
        release["failure"]["reason"]
            .as_str()
            .is_some_and(|reason| !reason.contains("[") && !reason.is_empty()),
        "the reason quotes the error without Orbit's marker: {release}"
    );
    assert_eq!(pair.owner_claims()[0]["claim"]["phase"], "revoked");
    assert!(
        comments_of(&pair.owner_task(&task)).contains(class),
        "the release comment names the class: {}",
        pair.owner_task(&task)
    );
    let claim = pair
        .follower
        .pull_leaf_claim(&leaf)
        .unwrap()
        .expect("claimed leaf");
    assert_eq!(
        claim.failure_class.map(|class| class.as_str()),
        Some(class),
        "`orbit run show <leaf>` exposes the class under its pull claim"
    );
    (pair, drain, pass)
}

/// Whether the drain's window still runs `crew`, and the exclusion naming it.
fn sol_exclusion(pair: &Pair, drain: &str) -> Option<CrewExclusion> {
    let window = pair
        .follower
        .pull_drain_crew_window(drain)
        .unwrap()
        .expect("a pull drain has a crew window");
    window
        .excluded
        .into_iter()
        .find(|exclusion| exclusion.crew == "sol")
}

/// A host-side failure releases the claim and excludes the leaf's crew for
/// the drain's window, so the same task is not pulled straight back to fail
/// the same way.
#[test]
fn host_side_failures_release_the_claim_and_exclude_the_crew_for_the_window() {
    if !isolated(
        module_path!(),
        "host_side_failures_release_the_claim_and_exclude_the_crew_for_the_window",
    ) {
        return;
    }
    for (error, class) in [
        (
            "[validation_environment] required validation 'make ci-fast' could not run: \
             python3 lacks tomllib",
            "environment",
        ),
        (
            "[owner_route_unavailable] the owner could not be reached from the sandboxed worker",
            "owner_route",
        ),
        (
            "[transient_failure] action pin check could not connect (status 000)",
            "transient",
        ),
        (
            "[provider_unavailable] claude provider authentication failure (HTTP 401)",
            "provider",
        ),
    ] {
        let (pair, drain, _) = a_released_failure(error, class);
        let exclusion = sol_exclusion(&pair, &drain)
            .unwrap_or_else(|| panic!("{class} excludes the crew for the window"));
        let source = if class == "provider" {
            CrewExclusionSource::ProviderUnavailable
        } else {
            CrewExclusionSource::LeafReleased
        };
        assert_eq!(exclusion.source, source, "{class}: {exclusion:?}");
        let task = &pair.tasks[0];
        assert!(
            exclusion.reason.contains(task.as_str()),
            "{class}: {exclusion:?}"
        );

        let pass = pair.pass(&drain);
        assert_eq!(pass["admitted"], 0, "{class}: {pass}");
        let last = pair.wire.calls("orbit.task.pull");
        let excluded = last.last().expect("the pass asked again")["crews"]["excluded"].clone();
        assert!(
            excluded.as_array().is_some_and(|excluded| excluded
                .iter()
                .any(|exclusion| exclusion["crew"] == "sol")),
            "{class}: the owner is told the crew is excluded: {excluded}"
        );
        assert_eq!(pair.owner_claims().len(), 1, "{class}: not pulled back");
    }
}

/// A red base is not the host's crew either: the claim is released and the
/// crew keeps running.
#[test]
fn a_baseline_red_failure_releases_the_claim_without_excluding_the_crew() {
    if !isolated(
        module_path!(),
        "a_baseline_red_failure_releases_the_claim_without_excluding_the_crew",
    ) {
        return;
    }
    let (pair, drain, _) = a_released_failure(
        "[baseline_red] `make ci-fast` fails on the base as well: rustfmt diff in listing.rs",
        "baseline_red",
    );
    assert_eq!(sol_exclusion(&pair, &drain), None);
}

/// The candidate's own failure, and a task its final recovery judged, still
/// block the task — typed, so `orbit run show` says which.
#[test]
fn a_candidate_failure_still_blocks_the_task() {
    if !isolated(module_path!(), "a_candidate_failure_still_blocks_the_task") {
        return;
    }
    let pair = Pair::with_crews(&[Some("sol")]);
    let drain = pair.run_drain();
    let leaf = pair.running_leaf(&drain, 1);
    let task = pair.claimed_task(&leaf);
    pair.leaf_fails_with(&leaf, "cargo test: 3 tests failed");

    let pass = settle_only(&pair, &drain);
    assert_eq!(pair.owner_status(&task), "blocked", "{pass}");
    let failed = &settlement_of(&pair, &leaf)["Fail"];
    assert_eq!(failed["failure"]["class"], "candidate", "{failed}");
    assert_eq!(pair.owner_claims()[0]["claim"]["phase"], "failed");
    assert_eq!(
        pair.follower
            .pull_leaf_claim(&leaf)
            .unwrap()
            .expect("claimed leaf")
            .failure_class,
        Some(ClaimFailureClass::Candidate)
    );
    assert_eq!(
        sol_exclusion(&pair, &drain),
        None,
        "a candidate failure is no host's"
    );
}

/// Cancelling one launched claimed leaf, as `orbit run cancel <leaf>` and the
/// dashboard's cancel do, gives its claim back with the operator's reason
/// rather than failing the task, and leaves the crew running.
#[test]
fn cancelling_a_launched_claimed_leaf_returns_its_task_to_backlog_with_the_reason() {
    if !isolated(
        module_path!(),
        "cancelling_a_launched_claimed_leaf_returns_its_task_to_backlog_with_the_reason",
    ) {
        return;
    }
    let pair = Pair::with_crews(&[Some("sol")]);
    let drain = pair.run_drain();
    let (leaf, worker) = pair.running_leaf_with_worker(&drain, 1);
    let task = pair.claimed_task(&leaf);

    let cancel = pair
        .follower
        .cancel_job_run_with_reason(&leaf, "operator", "cli", Some("wrong crew for this task"))
        .expect("cancel the leaf");
    assert_eq!(cancel.outcome, "cancelled", "{cancel:?}");
    assert!(worker.stopped(), "the leaf's worker is stopped");
    assert_eq!(pair.run_state(&leaf), JobRunState::Cancelled);

    let owner = pair.owner_task(&task);
    assert_eq!(owner["status"], "backlog", "{owner:#}");
    assert!(
        comments_of(&owner).contains("wrong crew for this task"),
        "{owner:#}"
    );
    let release = &settlement_of(&pair, &leaf)["Release"];
    assert_eq!(release["failure"]["class"], "operator_cancel", "{release}");
    assert_eq!(pair.owner_claims()[0]["claim"]["phase"], "revoked");
    assert_eq!(
        sol_exclusion(&pair, &drain),
        None,
        "a cancel excludes no crew"
    );
}

/// A task released twice within the budget window for failures that were
/// not the candidate's is blocked by the third, with one comment listing
/// every reason. An operator's cancel does not spend the budget.
#[test]
fn a_third_budgeted_release_within_a_day_blocks_the_task_with_every_reason() {
    if !isolated(
        module_path!(),
        "a_third_budgeted_release_within_a_day_blocks_the_task_with_every_reason",
    ) {
        return;
    }
    let pair = Pair::new(1);
    let task = pair.tasks[0].clone();
    let drain = pair.run_drain();
    let reasons = [
        "`make ci-fast` fails on the base: first",
        "`make ci-fast` fails on the base: second",
        "`make ci-fast` fails on the base: third",
    ];

    // An operator's cancel first: released, and not counted.
    let (cancelled, worker) = pair.running_leaf_with_worker(&drain, 1);
    pair.follower
        .cancel_job_run_with_reason(&cancelled, "operator", "cli", Some("rebalancing hosts"))
        .expect("cancel the leaf");
    assert!(worker.stopped());
    assert_eq!(pair.owner_status(&task), "backlog");

    for (n, reason) in reasons.iter().enumerate() {
        let leaf = pair.running_leaf(&drain, 1);
        assert_eq!(
            pair.claimed_task(&leaf),
            task,
            "the same task is pulled again"
        );
        pair.leaf_fails_with(&leaf, &format!("[baseline_red] {reason}"));
        let pass = settle_only(&pair, &drain);
        let expected = if n < 2 { "backlog" } else { "blocked" };
        assert_eq!(pair.owner_status(&task), expected, "release {n}: {pass}");
    }

    let owner = pair.owner_task(&task);
    let budget_comments: Vec<&Value> = owner["comments"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|comment| {
            comment
                .to_string()
                .contains("released this claim 2 times within 24h")
        })
        .collect();
    assert_eq!(budget_comments.len(), 1, "{owner:#}");
    let comment = budget_comments[0].to_string();
    for reason in reasons {
        assert!(
            comment.contains(reason),
            "every reason is listed: {comment}"
        );
    }
    assert!(
        !comment.contains("rebalancing hosts"),
        "an operator's cancel is not counted: {comment}"
    );
    let phases = pair
        .owner_claims()
        .iter()
        .map(|claim| claim["claim"]["phase"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        phases.iter().filter(|phase| *phase == "revoked").count(),
        3,
        "{phases:?}"
    );
    assert_eq!(
        phases.iter().filter(|phase| *phase == "failed").count(),
        1,
        "{phases:?}"
    );
}

/// The owner keeps a task a drain released for its host's failure from that
/// drain for the rest of its window, even when the drain's next request was
/// built before the release reached it and so excludes no crew. Another
/// drain may still take it.
#[test]
fn the_owner_does_not_hand_a_host_released_task_back_to_the_same_drain() {
    if !isolated(
        module_path!(),
        "the_owner_does_not_hand_a_host_released_task_back_to_the_same_drain",
    ) {
        return;
    }
    let (pair, _, _) = a_released_failure(
        "[validation_environment] required validation could not run: python3 lacks tomllib",
        "environment",
    );
    let task = pair.tasks[0].clone();
    let mut request = pair.wire.calls("orbit.task.pull")[0].clone();
    assert!(request["crews"].get("excluded").is_none(), "{request}");

    request["request_id"] = json!("built-before-the-release");
    let same = pair
        .wire
        .call("", "orbit.task.pull", request.clone())
        .unwrap();
    let receipt = &same["receipt"];
    assert!(receipt["claim"].is_null(), "{same:#}");
    let held = receipt["crew_unavailable"]
        .as_array()
        .and_then(|held| held.iter().find(|held| held["task_id"] == task.as_str()))
        .unwrap_or_else(|| panic!("the task is held from this drain: {same:#}"));
    assert!(
        held["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("environment")),
        "the hold names the release's class: {held}"
    );

    request["request_id"] = json!("another-drain");
    request["run_context"]["run_id"] = json!("jrun-another-drain");
    let other = pair.wire.call("", "orbit.task.pull", request).unwrap();
    assert_eq!(
        other["receipt"]["claim"]["task_id"],
        task.as_str(),
        "{other:#}"
    );
}
