//! [ORB-23003] An owner-local delivery that ends in review (a dashboard
//! ship, `orbit run ship`) has no handoff, so the landing never watched its
//! pull request: a red one sat in review with nothing on the task. The
//! review watch reads its checks and records the landing's classification.
use super::*;
use orbit_types::workflow::handoff::{
    HistoricalLandingChecksObservation, LANDING_CHECKS_OBSERVED_EVENT,
    LandingChecksObservationState,
};

const GRAPHQL_OUTAGE: &str = "GraphQL: Something went wrong while executing your query";

const TRANSIENT_SIGNATURE: &str = "ci-download: outcome=failed class=transient";
const TRANSIENT_DOWNLOAD_504: &str = "\
test\tTests\tcurl: (22) The requested URL returned error: 504
test\tTests\tci-download: attempt=9 curl_exit=22 http='504' seconds=0.400
test\tTests\tci-download: failure=total budget cannot accommodate backoff
test\tTests\tci-download: outcome=failed class=transient attempts=9 total_seconds=57.100 budget_seconds=60.0
test\tTests\t##[error]Process completed with exit code 1.
";

#[test]
fn a_review_delivery_with_failed_checks_records_their_classification() {
    isolated(
        "review_watch::a_review_delivery_with_failed_checks_records_their_classification",
        |sandbox| {
            let (fx, host) = fx_host(sandbox);
            fx.script_checks(&["failure"]);
            action(
                &host,
                "pr_open",
                &fx.open_input(&fx.candidate, &fx.base_sha),
            )
            .expect("open the delivery's pull request");
            host.set_status(TASK_ID, TaskStatus::Review);
            plant_run(
                &fx,
                &fx.candidate,
                10,
                "failure",
                "error: assertion failed: answer == 42\n",
            );
            plant_run(&fx, &fx.base_sha, 20, "success", "");
            let watch = json!({
                "task_ids": [TASK_ID],
                "pr_number": PR_NUMBER,
                "workspace_path": fx.repo,
                "base": BASE,
                "base_sync": "local",
                "max_wait_seconds": 30,
                "poll_interval_seconds": 5,
            });
            let watched = action(&host, "pr_checks_watch", &watch)
                .expect("the review watch is a shipped action");
            assert_eq!(watched["recorded"], true, "{watched}");
            let observed: Vec<HistoricalLandingChecksObservation> = host
                .history(TASK_ID)
                .iter()
                .filter_map(HistoricalLandingChecksObservation::from_history_entry)
                .collect();
            assert_eq!(observed.len(), 1, "one landing check event: {observed:?}");
            let recorded = &observed[0];
            let note = recorded
                .observation()
                .expect("the recorded note decodes as an observation");
            assert_eq!(
                note.state(),
                LandingChecksObservationState::Failed,
                "{note:?}"
            );
            assert_eq!(note.pr_number(), PR_NUMBER, "{note:?}");
            assert_eq!(
                note.classification(),
                Some("candidate"),
                "the landing's classification: green on the fork point, red on the head: {note:?}"
            );
            assert_eq!(
                note.failed_checks(),
                Some(&["test".to_string()][..]),
                "{note:?}"
            );
            // The note keeps the bytes the untyped writer produced, which the
            // dashboard parses and replay compares: this key order, `head_sha`
            // always present, absent optional fields omitted.
            let legacy = serde_json::to_string(&json!({
                "state": "failed",
                "pr_number": note.pr_number(),
                "head_sha": note.head_sha(),
                "failed_checks": note.failed_checks(),
                "classification": note.classification(),
                "detail": note.detail(),
            }))
            .unwrap();
            assert_eq!(
                recorded.replay_key().as_note(),
                legacy,
                "the note's bytes are unchanged"
            );
            assert_eq!(
                host.status(TASK_ID),
                TaskStatus::Review,
                "the watch never moves the task"
            );

            // A repeated watch of the same head records nothing new.
            action(&host, "pr_checks_watch", &watch).expect("watch again");
            let again = host
                .history(TASK_ID)
                .iter()
                .filter(|entry| entry.event == LANDING_CHECKS_OBSERVED_EVENT)
                .count();
            assert_eq!(again, 1, "an identical observation is recorded once");
        },
    );
}

fn open_review_delivery(fx: &Fixture, host: &DeliveryHost) {
    action(host, "pr_open", &fx.open_input(&fx.candidate, &fx.base_sha))
        .expect("open the delivery's pull request");
    host.set_status(TASK_ID, TaskStatus::Review);
}

fn watch_input(fx: &Fixture, max_wait_seconds: u64) -> Value {
    json!({
        "task_ids": [TASK_ID],
        "pr_number": PR_NUMBER,
        "workspace_path": fx.repo,
        "base": BASE,
        "base_sync": "local",
        "max_wait_seconds": max_wait_seconds,
        "poll_interval_seconds": 5,
    })
}

fn observations(host: &DeliveryHost) -> Vec<HistoricalLandingChecksObservation> {
    host.history(TASK_ID)
        .iter()
        .filter_map(HistoricalLandingChecksObservation::from_history_entry)
        .collect()
}

/// No call that changes the pull request, its branch or its checks reached the forge.
fn assert_no_mutation(fx: &Fixture) {
    let mutating: Vec<String> = fx
        .forge_calls()
        .into_iter()
        .filter(|call| {
            [
                "pr merge",
                "pr edit",
                "pr close",
                "pr comment",
                "pr ready",
                "run rerun",
            ]
            .iter()
            .any(|verb| call.starts_with(verb))
        })
        .collect();
    assert!(mutating.is_empty(), "the watch mutated: {mutating:?}");
}

/// A status read the provider fails transiently is an unknown observation:
/// the watch keeps polling, then reports what the next answered read shows.
#[test]
fn a_transient_status_read_failure_keeps_the_watch_polling() {
    isolated(
        "review_watch::a_transient_status_read_failure_keeps_the_watch_polling",
        |sandbox| {
            let (fx, host) = fx_host(sandbox);
            fx.script_checks(&["success"]);
            open_review_delivery(&fx, &host);
            // More failures than one read's own bounded retries can absorb.
            fx.script_status_failures(5, GRAPHQL_OUTAGE);
            let watched =
                with_completion_clock(|| action(&host, "pr_checks_watch", &watch_input(&fx, 600)))
                    .expect("a transient outage shorter than the watch is not a failure");
            assert_eq!(watched["recorded"], true, "{watched}");
            assert_eq!(
                fx.forge_state("status-faults").as_deref(),
                Some("0"),
                "every scripted failure went through the watcher's read path"
            );
            let observed = observations(&host);
            assert_eq!(observed.len(), 1, "{observed:?}");
            let note = observed[0].observation().expect("a decodable note");
            assert_eq!(
                note.state(),
                LandingChecksObservationState::Passed,
                "{note:?}"
            );
            assert_eq!(note.hold_reason(), None, "{note:?}");
            assert_eq!(host.status(TASK_ID), TaskStatus::Review);
            assert_no_mutation(&fx);
        },
    );
}

/// An outage that outlasts the watch records a held `pending` observation
/// naming nothing it did not see, and the run does not fail.
#[test]
fn a_transient_status_read_failure_through_the_deadline_holds_pending() {
    isolated(
        "review_watch::a_transient_status_read_failure_through_the_deadline_holds_pending",
        |sandbox| {
            let (fx, host) = fx_host(sandbox);
            fx.script_checks(&["success"]);
            open_review_delivery(&fx, &host);
            fx.script_status_failures(1_000_000, GRAPHQL_OUTAGE);
            let clock = std::rc::Rc::new(std::cell::Cell::new(1_000_000_u64));
            let now = std::rc::Rc::clone(&clock);
            let calls = std::cell::Cell::new(0_u32);
            let waited = std::rc::Rc::clone(&clock);
            let waits = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
            let recorded_waits = std::rc::Rc::clone(&waits);
            let mut input = watch_input(&fx, 60);
            input["poll_interval_seconds"] = json!(600);
            let watched = orbit_engine::test_support::with_prerequisite_clock(
                move || {
                    let count = calls.get();
                    calls.set(count + 1);
                    if count == 1 {
                        now.set(now.get() + 50_000);
                    }
                    now.get()
                },
                |_, _| {},
                || {
                    orbit_engine::test_support::with_vcs_retry_wait(
                        move |duration| {
                            let remaining = 1_060_000 - waited.get();
                            let millis = u64::try_from(duration.as_millis()).unwrap();
                            recorded_waits.borrow_mut().push((millis, remaining));
                            waited.set(waited.get() + millis);
                        },
                        || action(&host, "pr_checks_watch", &input),
                    )
                },
            )
            .expect("an outage through the deadline is a hold, not a failed run");
            let waits = waits.borrow();
            let &(last_wait, remaining) = waits.last().expect("the watch waited");
            assert!(
                remaining < 60_000,
                "the read consumed part of the budget: {waits:?}"
            );
            assert_eq!(
                last_wait, remaining,
                "the watch waits only to expiry: {waits:?}"
            );
            assert_eq!(clock.get(), 1_060_000, "the watch ends at expiry");
            assert_eq!(watched["recorded"], true, "{watched}");
            let observed = observations(&host);
            assert_eq!(observed.len(), 1, "{observed:?}");
            let note = observed[0].observation().expect("a decodable note");
            assert_eq!(
                note.state(),
                LandingChecksObservationState::Pending,
                "{note:?}"
            );
            assert_eq!(note.head_sha(), None, "{note:?}");
            assert_eq!(note.failed_checks(), None, "{note:?}");
            assert!(note.hold_reason().is_some(), "{note:?}");
            assert_eq!(host.status(TASK_ID), TaskStatus::Review);
            assert_no_mutation(&fx);
        },
    );
}

/// Leaving review in the final wait suppresses the held outage observation.
#[test]
fn leaving_review_during_the_final_outage_wait_does_not_record() {
    isolated(
        "review_watch::leaving_review_during_the_final_outage_wait_does_not_record",
        |sandbox| {
            let (fx, host) = fx_host(sandbox);
            let host = std::rc::Rc::new(host);
            fx.script_checks(&["success"]);
            open_review_delivery(&fx, &host);
            fx.script_status_failures(1_000_000, GRAPHQL_OUTAGE);
            let before = host.history(TASK_ID);
            let clock = std::rc::Rc::new(std::cell::Cell::new(1_000_000_u64));
            let now = std::rc::Rc::clone(&clock);
            let waited = std::rc::Rc::clone(&clock);
            let wait_host = std::rc::Rc::clone(&host);
            let mut input = watch_input(&fx, 60);
            input["poll_interval_seconds"] = json!(600);
            let watched = orbit_engine::test_support::with_prerequisite_clock(
                move || now.get(),
                |_, _| {},
                || {
                    orbit_engine::test_support::with_vcs_retry_wait(
                        move |duration| {
                            let millis = u64::try_from(duration.as_millis()).unwrap();
                            if waited.get() + millis >= 1_060_000 {
                                wait_host.set_status(TASK_ID, TaskStatus::Backlog);
                            }
                            waited.set(waited.get() + millis);
                        },
                        || action(&host, "pr_checks_watch", &input),
                    )
                },
            )
            .expect("the watch ends when the task leaves review");
            assert_eq!(clock.get(), 1_060_000);
            assert_eq!(host.status(TASK_ID), TaskStatus::Backlog);
            assert_eq!(watched["ended"], "task_left_review", "{watched}");
            assert_eq!(watched["recorded"], false, "{watched}");
            assert_eq!(host.history(TASK_ID), before);
            assert_no_mutation(&fx);
        },
    );
}

/// A permanent read failure is not an outage to wait out.
#[test]
fn a_permanent_status_read_failure_still_fails_promptly() {
    isolated(
        "review_watch::a_permanent_status_read_failure_still_fails_promptly",
        |sandbox| {
            let (fx, host) = fx_host(sandbox);
            fx.script_checks(&["success"]);
            open_review_delivery(&fx, &host);
            fx.script_status_failures(1_000_000, "gh: Bad credentials (HTTP 401)");
            let error =
                with_completion_clock(|| action(&host, "pr_checks_watch", &watch_input(&fx, 600)))
                    .expect_err("a credential refusal fails the watch");
            assert!(error.to_string().contains("Bad credentials"), "{error}");
            assert!(observations(&host).is_empty());
            assert_no_mutation(&fx);
        },
    );
}

/// A deadline expiring before or during the PR status read records a held
/// `pending` observation without manufacturing a head or check result.
#[test]
fn an_expired_deadline_before_or_during_status_read_holds_pending() {
    isolated(
        "review_watch::an_expired_deadline_before_or_during_status_read_holds_pending",
        |sandbox| {
            for mode in ["before_read", "during_read"] {
                in_fresh_row(sandbox, |row| {
                    let (fx, host) = fx_host(row);
                    fx.script_checks(&["success"]);
                    open_review_delivery(&fx, &host);
                    if mode == "during_read" {
                        fx.write_forge("status-hang", "");
                    }
                    let clock = std::rc::Rc::new(std::cell::Cell::new(1_000_000_u64));
                    let now = std::rc::Rc::clone(&clock);
                    let calls = std::cell::Cell::new(0_u32);
                    let jump_to = if mode == "before_read" {
                        1_060_000
                    } else {
                        1_059_700
                    };
                    let watched = orbit_engine::test_support::with_prerequisite_clock(
                        move || {
                            let count = calls.get();
                            calls.set(count + 1);
                            if count == 1 {
                                now.set(jump_to);
                            }
                            now.get()
                        },
                        |_, _| {},
                        || action(&host, "pr_checks_watch", &watch_input(&fx, 60)),
                    )
                    .expect("budget expiry inside or before status read holds pending");
                    assert_eq!(watched["recorded"], true, "{mode}: {watched}");
                    if mode == "before_read" {
                        assert_eq!(
                            fx.forge_state("reads"),
                            None,
                            "no status read spawns after deadline expiry"
                        );
                    }
                    let observed = observations(&host);
                    assert_eq!(observed.len(), 1, "{mode}: {observed:?}");
                    let note = observed[0].observation().expect("a decodable note");
                    assert_eq!(
                        note.state(),
                        LandingChecksObservationState::Pending,
                        "{mode}: {note:?}"
                    );
                    assert_eq!(note.head_sha(), None, "{mode}: {note:?}");
                    assert_eq!(note.failed_checks(), None, "{mode}: {note:?}");
                    assert!(note.hold_reason().is_some(), "{mode}: {note:?}");
                    assert_eq!(host.status(TASK_ID), TaskStatus::Review, "{mode}");
                    assert_no_mutation(&fx);
                });
            }
        },
    );
}

/// A pending status followed by expiry during the normal polling interval
/// records `pending` at the budget boundary without starting an expired read.
#[test]
fn a_pending_status_expiring_during_poll_wait_records_pending_at_boundary() {
    isolated(
        "review_watch::a_pending_status_expiring_during_poll_wait_records_pending_at_boundary",
        |sandbox| {
            let (fx, host) = fx_host(sandbox);
            fx.script_checks(&["merge_pending"]);
            open_review_delivery(&fx, &host);
            let clock = std::rc::Rc::new(std::cell::Cell::new(1_000_000_u64));
            let now = std::rc::Rc::clone(&clock);
            let calls = std::cell::Cell::new(0_u32);
            let waited = std::rc::Rc::clone(&clock);
            let waits = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
            let recorded_waits = std::rc::Rc::clone(&waits);
            let mut input = watch_input(&fx, 60);
            input["poll_interval_seconds"] = json!(600);
            let watched = orbit_engine::test_support::with_prerequisite_clock(
                move || {
                    let count = calls.get();
                    calls.set(count + 1);
                    if count == 1 {
                        now.set(1_050_000);
                    }
                    now.get()
                },
                |_, _| {},
                || {
                    orbit_engine::test_support::with_vcs_retry_wait(
                        move |duration| {
                            let remaining = 1_060_000 - waited.get();
                            let millis = u64::try_from(duration.as_millis()).unwrap();
                            recorded_waits.borrow_mut().push((millis, remaining));
                            waited.set(waited.get() + millis);
                        },
                        || action(&host, "pr_checks_watch", &input),
                    )
                },
            )
            .expect("pending expiry during poll wait records pending at the budget boundary");
            assert_eq!(
                waits.borrow().as_slice(),
                &[(10_000, 10_000)],
                "poll sleep is bounded by remaining budget"
            );
            assert_eq!(clock.get(), 1_060_000, "watch stops at the budget boundary");
            assert_eq!(
                fx.forge_state("reads").as_deref(),
                Some("1"),
                "no expired read starts after the poll wait reaches the deadline"
            );
            assert_eq!(watched["recorded"], true, "{watched}");
            let observed = observations(&host);
            assert_eq!(observed.len(), 1, "{observed:?}");
            let note = observed[0].observation().expect("a decodable note");
            assert_eq!(
                note.state(),
                LandingChecksObservationState::Pending,
                "{note:?}"
            );
            assert_eq!(note.head_sha(), Some(fx.candidate.as_str()), "{note:?}");
            assert_eq!(note.failed_checks(), None, "{note:?}");
            assert_eq!(note.hold_reason(), None, "{note:?}");
            assert_eq!(
                note.reason(),
                Some("the checks had not settled after 60s"),
                "{note:?}"
            );
            assert_eq!(host.status(TASK_ID), TaskStatus::Review);
            assert_no_mutation(&fx);
        },
    );
}

/// Leaving review during an expired status read or during the final pending
/// poll wait suppresses the observation and ends with `task_left_review`.
#[test]
fn leaving_review_before_read_expiry_or_during_pending_wait_does_not_record() {
    isolated(
        "review_watch::leaving_review_before_read_expiry_or_during_pending_wait_does_not_record",
        |sandbox| {
            for mode in ["during_read", "during_pending_wait"] {
                in_fresh_row(sandbox, |row| {
                    let (fx, host) = fx_host(row);
                    let host = std::rc::Rc::new(host);
                    fx.script_checks(&[if mode == "during_pending_wait" {
                        "merge_pending"
                    } else {
                        "success"
                    }]);
                    open_review_delivery(&fx, &host);
                    if mode == "during_read" {
                        fx.write_forge("status-hang", "");
                    }
                    let before = host.history(TASK_ID);
                    let clock = std::rc::Rc::new(std::cell::Cell::new(1_000_000_u64));
                    let now = std::rc::Rc::clone(&clock);
                    let calls = std::cell::Cell::new(0_u32);
                    let waited = std::rc::Rc::clone(&clock);
                    let probe_host = std::rc::Rc::clone(&host);
                    let wait_host = std::rc::Rc::clone(&host);
                    let mut input = watch_input(&fx, 60);
                    input["poll_interval_seconds"] = json!(600);
                    let watched = orbit_engine::test_support::with_prerequisite_clock(
                        move || {
                            let count = calls.get();
                            calls.set(count + 1);
                            if count == 1 {
                                now.set(if mode == "during_read" {
                                    1_059_700
                                } else {
                                    1_050_000
                                });
                            }
                            now.get()
                        },
                        move |_, _| {
                            if mode == "during_read" {
                                probe_host.set_status(TASK_ID, TaskStatus::Backlog);
                            }
                        },
                        || {
                            orbit_engine::test_support::with_vcs_retry_wait(
                                move |duration| {
                                    let millis = u64::try_from(duration.as_millis()).unwrap();
                                    if waited.get() + millis >= 1_060_000 {
                                        wait_host.set_status(TASK_ID, TaskStatus::Backlog);
                                    }
                                    waited.set(waited.get() + millis);
                                },
                                || action(&host, "pr_checks_watch", &input),
                            )
                        },
                    )
                    .expect("leaving review suppresses the pending entry");
                    assert_eq!(host.status(TASK_ID), TaskStatus::Backlog, "{mode}");
                    assert_eq!(watched["ended"], "task_left_review", "{mode}: {watched}");
                    assert_eq!(watched["recorded"], false, "{mode}: {watched}");
                    assert_eq!(host.history(TASK_ID), before, "{mode}");
                    assert_no_mutation(&fx);
                });
            }
        },
    );
}

fn failing_retry_watch(row: &Path) -> (Fixture, DeliveryHost) {
    let (fx, host, _) = super::ci_rerun::fixture(row);
    patch_retry_config(&fx, |config| {
        config["conclusions"] = json!({"lint": "success"});
        config["repeat"] = json!(true);
    });
    for job in [1000, 1010] {
        fs::write(
            fx.forge.join(format!("retry-log-{job}")),
            TRANSIENT_DOWNLOAD_504,
        )
        .unwrap();
    }
    (fx, host)
}

fn patch_retry_config(fx: &Fixture, edit: impl FnOnce(&mut Value)) {
    let path = fx.forge.join("retry-config.json");
    let mut config: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    edit(&mut config);
    fs::write(path, serde_json::to_vec(&config).unwrap()).unwrap();
}

fn assert_no_pr_or_branch_mutation(fx: &Fixture) {
    let mutating: Vec<String> = fx
        .forge_calls()
        .into_iter()
        .filter(|call| {
            ["pr merge", "pr edit", "pr close", "pr comment", "pr ready"]
                .iter()
                .any(|verb| call.starts_with(verb))
        })
        .collect();
    assert!(mutating.is_empty(), "the watch mutated: {mutating:?}");
}

/// Reaching the watch deadline after launching a CI rerun or entering
/// infrastructure retry backoff records a held `pending` observation naming
/// no head or check rather than the stale pre-retry failure.
#[test]
fn a_rerun_or_backoff_reaching_the_deadline_holds_pending() {
    isolated(
        "review_watch::a_rerun_or_backoff_reaching_the_deadline_holds_pending",
        |sandbox| {
            for mode in ["rerun", "backoff"] {
                in_fresh_row(sandbox, |row| {
                    let (fx, host) = failing_retry_watch(row);
                    {
                        let mut config = locked(&host.pr_config);
                        config.ci_infrastructure_signatures = vec![TRANSIENT_SIGNATURE.into()];
                        if mode == "backoff" {
                            config.ci_infrastructure_retry.backoff_seconds = 120;
                        }
                    }
                    patch_retry_config(&fx, |config| {
                        if mode == "rerun" {
                            config["repeat"] = json!(false);
                        } else {
                            config["fail_attempts"] = json!(2);
                            config["repeat"] = json!(true);
                        }
                    });
                    let mut input = watch_input(&fx, 60);
                    input["poll_interval_seconds"] = json!(if mode == "rerun" { 600 } else { 50 });
                    let clock = std::rc::Rc::new(std::cell::Cell::new(1_000_000_u64));
                    let read_clock = std::rc::Rc::clone(&clock);
                    let wait_clock = std::rc::Rc::clone(&clock);
                    let watched = orbit_engine::test_support::with_prerequisite_clock(
                        move || {
                            let now = read_clock.get();
                            read_clock.set(now + 1);
                            now
                        },
                        |_, _| {},
                        || {
                            orbit_engine::test_support::with_vcs_retry_wait(
                                move |duration| {
                                    let millis = u64::try_from(duration.as_millis()).unwrap();
                                    wait_clock.set(wait_clock.get() + millis);
                                },
                                || action(&host, "pr_checks_watch", &input),
                            )
                        },
                    )
                    .expect("expiry after retry or backoff holds pending");
                    assert!(
                        clock.get() >= 1_060_000,
                        "{mode}: watch reached deadline: {}",
                        clock.get()
                    );
                    assert_eq!(watched["recorded"], true, "{mode}: {watched}");
                    let observed = observations(&host);
                    assert_eq!(observed.len(), 1, "{mode}: {observed:?}");
                    let note = observed[0].observation().expect("a decodable note");
                    assert_eq!(
                        note.state(),
                        LandingChecksObservationState::Pending,
                        "{mode}: {note:?}"
                    );
                    assert_eq!(note.head_sha(), None, "{mode}: {note:?}");
                    assert_eq!(note.failed_checks(), None, "{mode}: {note:?}");
                    assert_eq!(
                        note.hold_reason(),
                        Some(
                            "the watch's deadline expired before the pull request's status was observed"
                        ),
                        "{mode}: {note:?}"
                    );
                    assert_eq!(
                        fx.forge_lines("retry-sends"),
                        vec!["run rerun 700 --failed".to_string()],
                        "{mode}"
                    );
                    if mode == "backoff" {
                        assert!(
                            host.history(TASK_ID).iter().any(|entry| {
                                entry.event
                                    == orbit_types::workflow::CI_INFRASTRUCTURE_RETRY_BACKOFF_EVENT
                            }),
                            "{mode}: backoff entry recorded"
                        );
                    }
                    assert_eq!(host.status(TASK_ID), TaskStatus::Review, "{mode}");
                    assert_no_pr_or_branch_mutation(&fx);
                });
            }
        },
    );
}

/// Reaching the watch deadline after deferring a throttled failed-job log
/// read records a held `pending` observation naming no head or check rather
/// than the stale pre-deferral failure.
#[test]
fn a_deferred_throttled_log_read_reaching_the_deadline_holds_pending() {
    isolated(
        "review_watch::a_deferred_throttled_log_read_reaching_the_deadline_holds_pending",
        |sandbox| {
            let (fx, host) = failing_retry_watch(sandbox);
            locked(&host.pr_config).ci_infrastructure_signatures = vec![TRANSIENT_SIGNATURE.into()];
            patch_retry_config(&fx, |config| {
                config["repeat"] = json!(false);
            });
            host.script(
                "ci.job_log",
                json!({
                    "fallback_error": "HTTP 403: API rate limit exceeded for user ID 270255973.",
                    "read_throttle": {
                        "retry_after_seconds": 10,
                        "principal": "user ID 270255973"
                    }
                }),
            );
            let mut input = watch_input(&fx, 60);
            input["poll_interval_seconds"] = json!(600);
            let clock = std::rc::Rc::new(std::cell::Cell::new(1_000_000_u64));
            let read_clock = std::rc::Rc::clone(&clock);
            let wait_clock = std::rc::Rc::clone(&clock);
            let waits = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
            let recorded_waits = std::rc::Rc::clone(&waits);
            let watched = orbit_engine::test_support::with_prerequisite_clock(
                move || {
                    let now = read_clock.get();
                    read_clock.set(now + 1);
                    now
                },
                |_, _| {},
                || {
                    orbit_engine::test_support::with_vcs_retry_wait(
                        move |duration| {
                            let millis = u64::try_from(duration.as_millis()).unwrap();
                            recorded_waits.borrow_mut().push(millis);
                            wait_clock.set(wait_clock.get() + millis);
                        },
                        || action(&host, "pr_checks_watch", &input),
                    )
                },
            )
            .expect("expiry after deferred log read holds pending");
            assert_eq!(waits.borrow().len(), 2, "deferral wait then poll wait");
            assert_eq!(waits.borrow()[0], 10_000, "honors provider retry-after");
            assert!(clock.get() >= 1_060_000, "{}", clock.get());
            assert_eq!(watched["recorded"], true, "{watched}");
            let observed = observations(&host);
            assert_eq!(observed.len(), 1, "{observed:?}");
            let note = observed[0].observation().expect("a decodable note");
            assert_eq!(
                note.state(),
                LandingChecksObservationState::Pending,
                "{note:?}"
            );
            assert_eq!(note.head_sha(), None, "{note:?}");
            assert_eq!(note.failed_checks(), None, "{note:?}");
            assert_eq!(
                note.hold_reason(),
                Some("the watch's deadline expired before the pull request's status was observed"),
                "{note:?}"
            );
            assert!(fx.forge_lines("retry-sends").is_empty());
            assert_eq!(
                host.history(TASK_ID)
                    .iter()
                    .filter(|entry| entry.event == "pr_auto_fix_read_deferred")
                    .count(),
                1
            );
            assert_eq!(host.status(TASK_ID), TaskStatus::Review);
            assert_no_mutation(&fx);
        },
    );
}

/// Leaving review during the final wait after a rerun, backoff, or deferred
/// throttled read suppresses the held observation and ends with
/// `task_left_review`.
#[test]
fn leaving_review_during_retry_or_deferred_wait_does_not_record() {
    isolated(
        "review_watch::leaving_review_during_retry_or_deferred_wait_does_not_record",
        |sandbox| {
            for mode in ["rerun", "backoff", "deferred_read"] {
                in_fresh_row(sandbox, |row| {
                    let (fx, host) = failing_retry_watch(row);
                    {
                        let mut config = locked(&host.pr_config);
                        config.ci_infrastructure_signatures = vec![TRANSIENT_SIGNATURE.into()];
                        if mode == "backoff" {
                            config.ci_infrastructure_retry.backoff_seconds = 120;
                        }
                    }
                    patch_retry_config(&fx, |config| {
                        if mode == "backoff" {
                            config["fail_attempts"] = json!(2);
                            config["repeat"] = json!(true);
                        } else {
                            config["repeat"] = json!(false);
                        }
                    });
                    if mode == "deferred_read" {
                        host.script(
                            "ci.job_log",
                            json!({
                                "fallback_error": "HTTP 403: API rate limit exceeded for user ID 270255973.",
                                "read_throttle": {
                                    "retry_after_seconds": 10,
                                    "principal": "user ID 270255973"
                                }
                            }),
                        );
                    }
                    let host = std::rc::Rc::new(host);
                    let mut input = watch_input(&fx, 60);
                    input["poll_interval_seconds"] =
                        json!(if mode == "backoff" { 50 } else { 600 });
                    let clock = std::rc::Rc::new(std::cell::Cell::new(1_000_000_u64));
                    let read_clock = std::rc::Rc::clone(&clock);
                    let wait_clock = std::rc::Rc::clone(&clock);
                    let wait_host = std::rc::Rc::clone(&host);
                    let watched = orbit_engine::test_support::with_prerequisite_clock(
                        move || {
                            let now = read_clock.get();
                            read_clock.set(now + 1);
                            now
                        },
                        |_, _| {},
                        || {
                            orbit_engine::test_support::with_vcs_retry_wait(
                                move |duration| {
                                    let millis = u64::try_from(duration.as_millis()).unwrap();
                                    if wait_clock.get() + millis >= 1_060_000 {
                                        wait_host.set_status(TASK_ID, TaskStatus::Backlog);
                                    }
                                    wait_clock.set(wait_clock.get() + millis);
                                },
                                || action(host.as_ref(), "pr_checks_watch", &input),
                            )
                        },
                    )
                    .expect("leaving review suppresses the held observation");
                    assert_eq!(host.status(TASK_ID), TaskStatus::Backlog, "{mode}");
                    assert_eq!(watched["ended"], "task_left_review", "{mode}: {watched}");
                    assert_eq!(watched["recorded"], false, "{mode}: {watched}");
                    assert!(
                        observations(&host).is_empty(),
                        "{mode}: no observation recorded: {:?}",
                        observations(&host)
                    );
                    assert_no_pr_or_branch_mutation(&fx);
                });
            }
        },
    );
}

/// One push run on `sha` whose `test` job concluded `conclusion`, served to
/// the substitute `gh` as the commit's jobs and failed-job log.
fn plant_run(fx: &Fixture, sha: &str, run_id: u64, conclusion: &str, log: &str) {
    let dir = fx.forge.join("commit-ci").join(sha);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("run_id"), run_id.to_string()).unwrap();
    let runs = json!({
        "total_count": 1,
        "workflow_runs": [{
            "id": run_id, "name": "CI", "workflow_id": 1, "event": "push",
            "head_sha": sha, "head_branch": BASE
        }]
    });
    fs::write(dir.join("runs.json"), serde_json::to_vec(&runs).unwrap()).unwrap();
    let jobs = json!({
        "total_count": 1,
        "jobs": [{
            "id": run_id + 1, "name": "test", "status": "completed", "conclusion": conclusion,
            "steps": [{"name": "compile", "conclusion": conclusion}]
        }]
    });
    fs::write(dir.join("jobs.json"), serde_json::to_vec(&jobs).unwrap()).unwrap();
    fs::write(dir.join("log"), log).unwrap();
}
