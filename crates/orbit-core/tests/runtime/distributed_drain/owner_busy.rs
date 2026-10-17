//! A claimed leaf whose owner call failed because the owner's store stayed
//! locked is released without blaming the executing host or its crew: the
//! drain backs its claims off for a bounded time, with a typed readiness
//! reason, and claims again in the same window. Only an owner that stays
//! busy across repeated backoffs suppresses the host, and the reason names
//! how often.

use chrono::{Duration, SubsecRound};
use orbit_types::workflow::ClaimFailureClass;

use super::*;

/// What the follower's SSH session reports when the owner's `orbit` exits at
/// startup on its locked store, before it answers `initialize`.
const LOCKED: &str = "session ended before answering 'initialize'; ssh stderr: error: store \
                      error: database is locked";

/// An owner route whose every session finds the owner's store locked.
struct LockedOwner;

impl OwnerCoordinator for LockedOwner {
    fn call(
        &self,
        _name: &str,
        _input: Value,
        _session: ToolSessionContext,
    ) -> Result<Value, OrbitError> {
        Err(OrbitError::RemoteTool {
            code: orbit_common::LOCK_BUSY_ERROR_CODE.into(),
            message: format!("{OWNER}: {LOCKED}"),
            payload: json!({"code": orbit_common::LOCK_BUSY_ERROR_CODE, "message": LOCKED}),
        })
    }
}

/// The step failure `leaf`'s bound worker records when its owner read keeps
/// finding the owner's store locked past the read's bounded retries.
fn locked_owner_failure(pair: &Pair, leaf: &str) -> String {
    let record = pair.admission(leaf);
    let claim = record.receipt.as_ref().unwrap().claim.clone().unwrap();
    let bound = pair
        .follower
        .clone()
        .with_worker_invocation(
            orbit_types::tool::WorkerInvocation {
                owner_machine_id: OWNER.into(),
                owner_workspace_id: record.destination.owner_workspace_id.clone(),
                owner_destination: record.destination.selector.clone(),
                task_id: claim.task_id.clone(),
                claim_id: claim.claim_id.clone(),
                execution: claim.executed_on.clone(),
                bound_run_id: leaf.to_string(),
            },
            Arc::new(LockedOwner),
        )
        .unwrap();
    let error = orbit_core::test_support::with_owner_read_retry_wait(
        |_| {},
        || bound.get_task(&claim.task_id),
    )
    .expect_err("the owner's store stays locked");
    assert!(error.is_lock_busy(), "{error:?}");
    let message = error.to_string();
    assert_eq!(
        ClaimFailureClass::of_step_failure(None, Some(&message)),
        Some(ClaimFailureClass::Transient),
        "a busy owner is not a lost owner route: {message}"
    );
    message
}

/// The failure the follower delivered with `leaf`'s release.
fn released_failure(pair: &Pair, leaf: &str) -> Value {
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
        .map(|settle| settle["settlement"]["Release"]["failure"].clone())
        .expect("the leaf's release was delivered")
}

/// The first busy-owner release backs off and passes; the drain claims the
/// same task again on the same crew. The second one, still in its backoff,
/// holds requests with an `owner_busy_backoff` reason naming when they
/// resume, and the host is not suppressed.
fn a_busy_owner_release_backs_the_drain_off_and_it_claims_again_in_the_window() {
    let pair = Pair::with_crews(&[Some("sol")]);
    let drain = pair.run_drain();
    let first = pair.running_leaf(&drain, 1);
    let task = pair.claimed_task(&first);
    let locked = locked_owner_failure(&pair, &first);
    // Its five-minute backoff passed a minute ago.
    pair.leaf_fails_at(&first, &locked, Utc::now() - Duration::minutes(6));

    let second = pair.running_leaf(&drain, 1);
    assert_ne!(second, first);
    assert_eq!(
        pair.claimed_task(&second),
        task,
        "the owner hands the released task back to the same drain"
    );
    let failure = released_failure(&pair, &first);
    assert_eq!(failure["class"], "transient", "{failure}");
    assert_eq!(failure["owner_busy"], true, "{failure}");
    let window = pair
        .follower
        .pull_drain_crew_window(&drain)
        .unwrap()
        .expect("a pull drain has a crew window");
    assert_eq!(window.host_suppressed, None, "{window:#?}");
    assert_eq!(window.owner_busy_backoff, None, "{window:#?}");
    assert!(window.excluded.is_empty(), "the crew is kept: {window:#?}");

    let ended = Utc::now().trunc_subsecs(0);
    pair.leaf_fails_at(&second, &locked_owner_failure(&pair, &second), ended);
    let settled = pair.pass(&drain);
    assert_eq!(settled["admitted"], 0, "{settled}");
    let pulls = pair.wire.calls("orbit.task.pull").len();

    let pass = pair.pass(&drain);
    assert_eq!(pass["admitted"], 0, "{pass}");
    assert!(pass["crews"]["host_suppressed"].is_null(), "{pass}");
    let backoff = &pass["crews"]["owner_busy_backoff"];
    assert_eq!(backoff["episodes"], 2, "{pass}");
    assert_eq!(backoff["releases"], 2, "{pass}");
    let until = ended + Duration::minutes(5);
    assert_eq!(
        serde_json::from_value::<chrono::DateTime<Utc>>(backoff["until"].clone()).ok(),
        Some(until),
        "{pass}"
    );
    let refusal = pass["refusal"].as_str().unwrap_or_default();
    assert!(
        refusal.starts_with("owner_busy_backoff:") && refusal.contains(&until.to_rfc3339()),
        "{pass}"
    );
    assert_eq!(
        pair.wire.calls("orbit.task.pull").len(),
        pulls,
        "a backed-off drain asks the owner for nothing: {pass}"
    );
    let recorded = pair
        .follower
        .read_run_state(&drain)
        .unwrap()
        .and_then(|state| state.pull_drain.drain_last_pass)
        .and_then(|last| last.admission_refusal);
    assert_eq!(
        recorded.as_deref(),
        Some(refusal),
        "readiness shows the backoff"
    );
    assert_eq!(pair.owner_status(&task), "backlog");
}

/// Three busy-owner episodes, each after the previous backoff passed,
/// suppress the host for the window with a reason naming the count. None of
/// the releases spends the task's release budget, so the task stays in the
/// backlog where a third budgeted release would block it.
fn busy_owner_releases_past_the_episode_bound_suppress_the_host_naming_the_count() {
    let pair = Pair::with_crews(&[Some("sol")]);
    let drain = pair.run_drain();
    let mut task = None;
    for minutes_ago in [30, 20, 10] {
        let leaf = pair.running_leaf(&drain, 1);
        task.get_or_insert_with(|| pair.claimed_task(&leaf));
        let locked = locked_owner_failure(&pair, &leaf);
        pair.leaf_fails_at(&leaf, &locked, Utc::now() - Duration::minutes(minutes_ago));
    }
    let settled = pair.pass(&drain);
    assert_eq!(settled["admitted"], 0, "{settled}");

    let window = pair
        .follower
        .pull_drain_crew_window(&drain)
        .unwrap()
        .expect("a pull drain has a crew window");
    let suppressed = window
        .host_suppressed
        .clone()
        .unwrap_or_else(|| panic!("repeated busy episodes suppress the host: {window:#?}"));
    assert!(
        suppressed.contains("3 backoffs") && suppressed.contains("3 releases"),
        "{suppressed}"
    );
    assert!(
        window.host_suppressed_by_busy_owner,
        "the remedy is on the owner, not this host: {window:#?}"
    );
    let pulls = pair.wire.calls("orbit.task.pull").len();
    let pass = pair.pass(&drain);
    assert!(
        pass["refusal"]
            .as_str()
            .is_some_and(|refusal| refusal.starts_with("host_suppressed:")),
        "{pass}"
    );
    assert_eq!(pair.wire.calls("orbit.task.pull").len(), pulls, "{pass}");
    assert_eq!(
        pair.owner_status(task.as_deref().unwrap()),
        "backlog",
        "busy-owner releases are outside the release budget"
    );
}

/// The owner bounds the busy-owner exemption per task: within the release
/// budget's day, a task's first three busy releases are outside the budget,
/// and every later one counts, so a task the owner keeps failing cannot
/// cycle forever across drains and followers. Each release here comes from a
/// fresh drain, whose own window never reaches its episode bound.
fn the_owner_counts_a_tasks_busy_releases_past_its_allowance_against_the_budget() {
    let pair = Pair::with_crews(&[Some("sol")]);
    let task = pair.tasks[0].clone();
    // Three exempt, then the budget's two, then the release that exceeds it.
    for release in 1..=6 {
        let drain = pair.run_drain();
        let leaf = pair.running_leaf(&drain, 1);
        assert_eq!(pair.claimed_task(&leaf), task, "release {release}");
        let locked = locked_owner_failure(&pair, &leaf);
        pair.leaf_fails_at(&leaf, &locked, Utc::now() - Duration::minutes(10));
        let pass = pair.pass_over(&drain, json!({"window_expired": true}));
        let expected = if release < 6 { "backlog" } else { "blocked" };
        assert_eq!(
            pair.owner_status(&task),
            expected,
            "release {release}: {pass}"
        );
    }
}

/// The owner honours the busy-owner mark only on a `transient` release. A
/// release of another class that carries it is an ordinary release of that
/// class: an `owner_route` one still holds the task from the drain that
/// released it, where a `transient` busy one does not.
fn the_owner_honours_the_busy_owner_mark_only_on_a_transient_release() {
    for (class, held) in [
        (ClaimFailureClass::Transient, false),
        (ClaimFailureClass::OwnerRoute, true),
    ] {
        let class_name = class.as_str();
        let pair = Pair::with_crews(&[Some("sol")]);
        let drain = pair.run_drain();
        let leaf = pair.running_leaf(&drain, 1);
        let record = pair.admission(&leaf);
        let claim = record.receipt.unwrap().claim.unwrap();
        let settlement = ClaimMutation::Release(ClaimEvidence {
            summary: Some("released by follower drain: the owner's store stayed locked".into()),
            failure: Some(orbit_store::contracts::ClaimFailure {
                class,
                reason: "database is locked".into(),
                crew: Some("sol".into()),
                candidate: None,
                provider_limit: false,
                provider_refusal: false,
                owner_busy: true,
            }),
            ..ClaimEvidence::default()
        });
        pair.wire
            .call(
                "",
                "orbit.drain.claim.settle",
                json!({"claim_id": claim.claim_id, "run_id": leaf, "settlement": settlement}),
            )
            .unwrap_or_else(|error| panic!("{class_name}: {error}"));
        let class = class_name;
        assert_eq!(pair.owner_status(&claim.task_id), "backlog", "{class}");

        let mut request = pair.wire.calls("orbit.task.pull")[0].clone();
        request["request_id"] = json!(format!("after-the-{class}-release"));
        let answer = pair.wire.call("", "orbit.task.pull", request).unwrap();
        let held_from_drain = answer["receipt"]["crew_unavailable"]
            .as_array()
            .is_some_and(|held| {
                held.iter()
                    .any(|entry| entry["task_id"] == claim.task_id.as_str())
            });
        assert_eq!(held_from_drain, held, "{class}: {answer:#}");
        assert_eq!(
            answer["receipt"]["claim"]["task_id"].is_string(),
            !held,
            "{class}: {answer:#}"
        );
    }
}

crate::scenario_table!(
    a_busy_owner_release_backs_the_drain_off_and_it_claims_again_in_the_window_scenarios,
    a_busy_owner_release_backs_the_drain_off_and_it_claims_again_in_the_window,
    busy_owner_releases_past_the_episode_bound_suppress_the_host_naming_the_count,
    the_owner_counts_a_tasks_busy_releases_past_its_allowance_against_the_budget,
    the_owner_honours_the_busy_owner_mark_only_on_a_transient_release
);
