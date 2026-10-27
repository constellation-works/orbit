//! Blocked, failed and abandoned attempts in the backlog forecast
//! [ORB-18688], from dated status histories at the HTTP boundary.

use chrono::{DateTime, Duration, Utc};
use orbit_core::StoredBacklogForecast;
use orbit_core::application::task::TaskAddParams;
use orbit_types::task::{TaskComplexity, TaskEventRowV2, TaskStatus};
use serde_json::{Value, json};

use super::backlog_forecast::{
    ENDPOINT, backlog_task, completed, drain_hours, initial_work_forecast_baseline,
    one_hour_history, prediction,
};
use super::support::{Fixture, isolated, json_ok};

/// A medium task now in `status`, whose recorded history is `steps`: each
/// the status it entered and when, after its creation in backlog.
fn dated_task(
    fixture: &Fixture,
    status: TaskStatus,
    steps: &[(DateTime<Utc>, TaskStatus)],
) -> String {
    let id = fixture
        .runtime
        .add_task(TaskAddParams {
            title: "Observed attempts".into(),
            complexity: TaskComplexity::Medium,
            status: Some(status),
            ..Default::default()
        })
        .unwrap()
        .id
        .to_string();
    let registry = orbit_store::maintenance::task_registry::TaskRegistryStore::open(
        &orbit_store::maintenance::task_registry::task_registry_path(
            &fixture.runtime.global_root(),
        ),
    )
    .unwrap();
    let events_path = registry
        .find_task_binding(&id)
        .unwrap()
        .unwrap()
        .canonical_path
        .join("events.jsonl");
    let created: TaskEventRowV2 = serde_json::from_str(
        std::fs::read_to_string(&events_path)
            .unwrap()
            .lines()
            .next()
            .unwrap(),
    )
    .unwrap();
    let mut rows = vec![TaskEventRowV2 {
        at: steps[0].0 - Duration::minutes(1),
        to_status: Some(TaskStatus::Backlog),
        ..created.clone()
    }];
    let mut from = TaskStatus::Backlog;
    for (index, (at, to)) in steps.iter().enumerate() {
        rows.push(TaskEventRowV2 {
            event_id: format!("{}-{index}", created.event_id),
            at: *at,
            event_type: "status_changed".into(),
            note: None,
            from_status: Some(from),
            to_status: Some(*to),
            ..created.clone()
        });
        from = *to;
    }
    let events: String = rows
        .iter()
        .map(|row| serde_json::to_string(row).unwrap() + "\n")
        .collect();
    std::fs::write(events_path, events).unwrap();
    id
}

/// Ten clean one-hour deliveries ten days ago and, with `blocking`, ten
/// attempts that blocked after two hours and returned a day later, plus
/// two waits that never returned within 90 days.
fn attempt_history(fixture: &Fixture, blocking: bool) {
    one_hour_history(fixture);
    let now = Utc::now();
    let at = now - Duration::days(10);
    let hours = |count: i64| at + Duration::hours(count);
    for _ in 0..10 {
        dated_task(
            fixture,
            TaskStatus::Done,
            &[
                (at, TaskStatus::InProgress),
                (hours(1), TaskStatus::Review),
                (hours(2), TaskStatus::Done),
            ],
        );
    }
    if !blocking {
        return;
    }
    for _ in 0..10 {
        dated_task(
            fixture,
            TaskStatus::Done,
            &[
                (at, TaskStatus::InProgress),
                (hours(2), TaskStatus::Blocked),
                (hours(26), TaskStatus::Backlog),
                (hours(27), TaskStatus::InProgress),
                (hours(28), TaskStatus::Review),
                (hours(29), TaskStatus::Done),
            ],
        );
    }
    for _ in 0..2 {
        dated_task(
            fixture,
            TaskStatus::Rejected,
            &[
                (now - Duration::days(100), TaskStatus::Blocked),
                (now - Duration::days(1), TaskStatus::Rejected),
            ],
        );
    }
}

fn forecast(fixture: &Fixture) -> Value {
    initial_work_forecast_baseline(json_ok(
        fixture
            .server(false)
            .get(&format!("{ENDPOINT}&concurrency=1")),
    ))
}

#[test]
fn unresolved_paths_make_critical_path_unknown() {
    isolated(
        "backlog_outcomes::unresolved_paths_make_critical_path_unknown",
        || {
            let fixture = Fixture::new();
            one_hour_history(&fixture);
            let blocked = dated_task(
                &fixture,
                TaskStatus::Blocked,
                &[(Utc::now() - Duration::hours(1), TaskStatus::Blocked)],
            );
            let unknown = json!({"p50": null, "p90": null});
            let all_unfinished = forecast(&fixture);
            assert_eq!(
                prediction(&all_unfinished, &blocked)["completion_probability"],
                0.0
            );
            assert_eq!(all_unfinished["forecast"]["critical_path_hours"], unknown);

            let finite = backlog_task(&fixture, "Finite", &["file:finite.rs"], &[], &[]);
            let dependent = backlog_task(
                &fixture,
                "Dependent",
                &["file:dependent.rs"],
                &[],
                &[&blocked],
            );
            let mixed = forecast(&fixture);
            assert_eq!(prediction(&mixed, &finite)["completion_probability"], 1.0);
            assert_eq!(
                prediction(&mixed, &dependent)["completion_probability"],
                0.0
            );
            assert_eq!(mixed["forecast"]["critical_path_hours"], unknown);
        },
    );
}

#[test]
fn block_history_frees_slots_requeues_and_leaves_work_undrained() {
    isolated(
        "backlog_outcomes::block_history_frees_slots_requeues_and_leaves_work_undrained",
        || {
            let clean = Fixture::new();
            attempt_history(&clean, false);
            let first = backlog_task(&clean, "First", &["file:a.rs"], &[], &[]);
            let second = backlog_task(&clean, "Second", &["file:b.rs"], &[], &[]);
            let clean = forecast(&clean);
            // Outcomes arrived in model 5; later factors retain them.
            assert!(clean["model_version"].as_u64().unwrap() >= 5);
            assert_eq!(clean["inputs"]["admission_factors"]["outcomes"], 1);
            assert_eq!(
                clean["inputs"]["outcomes"]["episodes"],
                json!({"delivered": 10})
            );
            assert_eq!(drain_hours(&clean), (2.0, 2.0), "{}", clean["forecast"]);
            assert_eq!(clean["forecast"]["not_expected_to_drain"], 0.0);
            assert_eq!(clean["forecast"]["outcomes"]["blocked_attempts"], 0.0);
            assert_eq!(
                prediction(&clean, &first)["predicted_start_hours"]["p90"],
                0.0
            );
            assert_eq!(
                prediction(&clean, &second)["predicted_start_hours"]["p90"],
                1.0
            );

            let fixture = Fixture::new();
            attempt_history(&fixture, true);
            let first = backlog_task(&fixture, "First", &["file:a.rs"], &[], &[]);
            let second = backlog_task(&fixture, "Second", &["file:b.rs"], &[], &[]);
            let blocking = forecast(&fixture);
            let outcomes = &blocking["inputs"]["outcomes"];
            // Attempts of tasks that never reached done are sampled too.
            assert_eq!(
                outcomes["episodes"],
                json!({"delivered": 20, "blocked": 10})
            );
            assert_eq!(
                outcomes["returns"],
                json!({"waits": 12, "returned": 10, "resolved": 0, "absorbed": 0, "censored": 2})
            );
            let p50 = blocking["forecast"]["time_to_drain_hours"]["p50"].as_f64();
            assert!(
                p50.is_none_or(|p50| p50 > 2.0),
                "a third of attempts block, so most drains wait for a return: {}",
                blocking["forecast"]
            );
            let undrained = blocking["forecast"]["not_expected_to_drain"]
                .as_f64()
                .unwrap();
            assert!(
                undrained > 0.0,
                "a sixth of blocks never return: {}",
                blocking["forecast"]
            );
            assert!(blocking["forecast"]["outcomes"]["blocked_attempts"].as_f64() > Some(0.0));
            assert!(blocking["forecast"]["wait_hours"]["return"]["p90"].as_f64() > Some(0.0));
            // A blocked first attempt frees the only slot at its two-hour
            // hold, so the second task starts then rather than after the
            // first task's return and delivery.
            let started = &prediction(&blocking, &second)["predicted_start_hours"];
            assert_eq!(
                (started["p50"].as_f64(), started["p90"].as_f64()),
                (Some(1.0), Some(2.0))
            );
            let returned = prediction(&blocking, &first)["completion_probability"]
                .as_f64()
                .unwrap();
            assert!(
                returned > 0.8 && returned < 1.0,
                "a blocked attempt requeues after its return: {returned}"
            );

            // A task blocked now starts out waiting for the rest of its
            // return: the only return observed came a day after blocking.
            let now = Utc::now();
            dated_task(
                &fixture,
                TaskStatus::Blocked,
                &[(now - Duration::hours(1), TaskStatus::Blocked)],
            );
            let with_blocked = forecast(&fixture);
            assert_eq!(with_blocked["coverage"]["blocked"], 1);
            let blocked = with_blocked["forecast"]["task_predictions"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["status"] == "blocked")
                .unwrap();
            let start = blocked["predicted_start_hours"]["p50"].as_f64().unwrap();
            assert!((start - 23.0).abs() < 0.1, "{blocked}");
            assert!(
                with_blocked["forecast"]["not_expected_to_drain"]
                    .as_f64()
                    .unwrap()
                    > undrained
            );
        },
    );
}

#[test]
fn abandoned_and_blocked_tasks_do_not_corrupt_settled_actuals() {
    isolated(
        "backlog_outcomes::abandoned_and_blocked_tasks_do_not_corrupt_settled_actuals",
        || {
            let fixture = Fixture::new();
            let now = Utc::now();
            let done_at = now - Duration::days(2);
            let delivered = completed(&fixture, done_at);
            let rejected = fixture
                .runtime
                .add_task(TaskAddParams {
                    title: "Abandoned".into(),
                    complexity: TaskComplexity::Medium,
                    status: Some(TaskStatus::Rejected),
                    ..Default::default()
                })
                .unwrap()
                .id
                .to_string();
            let blocked = fixture
                .runtime
                .add_task(TaskAddParams {
                    title: "Still blocked".into(),
                    complexity: TaskComplexity::Medium,
                    status: Some(TaskStatus::Blocked),
                    ..Default::default()
                })
                .unwrap()
                .id
                .to_string();
            let quantiles = json!({"p50": 1.0, "p90": 2.0});
            let store = |version: u32, minutes: i64, ids: &[&str]| {
                let tasks: Vec<Value> = ids
                    .iter()
                    .map(|id| json!({"id": id, "complexity": "medium", "forecast": {"hours": quantiles}}))
                    .collect();
                assert!(
                    fixture
                        .runtime
                        .record_backlog_forecast(
                            StoredBacklogForecast {
                                workspace_id: String::new(),
                                forecast_at: done_at
                                    - Duration::hours(1)
                                    - Duration::minutes(minutes),
                                model_version: version,
                                inputs_json: "{}".into(),
                                result_json: json!({"time_to_drain_hours": quantiles}).to_string(),
                                tasks_json: Value::Array(tasks).to_string(),
                            },
                            Duration::minutes(1),
                        )
                        .unwrap()
                );
            };
            store(4, 180, &[&delivered, &rejected]);
            store(5, 120, &[&delivered, &rejected]);
            store(5, 60, &[&delivered, &blocked]);

            let calibration = &forecast(&fixture)["calibration"];
            let row = |version: u64, pending: u64| {
                calibration["forecasts"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|row| row["model_version"] == version && row["pending"] == pending)
                    .unwrap()
                    .clone()
            };
            // Model 4 keeps its exclusion.
            let old = row(4, 0);
            assert_eq!(old["excluded"]["rejected"], 1);
            assert_eq!(old["scored_whole"], false);
            // Model 5 settles an abandoned cohort without draining it: no
            // drain time from the task that happened to finish.
            let abandoned = row(5, 0);
            assert_eq!(abandoned["excluded"]["rejected"], 0);
            assert_eq!(abandoned["abandoned"], 1);
            assert_eq!(abandoned["settled"], true);
            assert_eq!(abandoned["scored_whole"], true);
            assert_eq!(abandoned["actual"]["drained"], false);
            assert_eq!(abandoned["actual"]["time_to_drain_hours"], Value::Null);
            assert_eq!(abandoned["actual"]["usd"], 1.0);
            // A blocked task is still pending.
            let pending = row(5, 1);
            assert_eq!(pending["pending"], 1);
            assert_eq!(pending["settled"], false);
            // Model 5's own calibration, which later versions keep apart.
            let five = calibration["by_model_version"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["model_version"] == 5)
                .unwrap();
            let current = &five["forecast_level"];
            assert_eq!(current["time_to_drain_hours"]["n"], 0);
            assert_eq!(current["settled"], 1);
            assert_eq!(current["not_drained"], 1);
            // The delivered task is scored in both model-5 forecasts.
            assert_eq!(five["task_level"]["hours"]["n"], 2);
        },
    );
}

/// One blocked episode that returns `delay` after leaving the queue, then
/// one delivery. A zero delay is a return at the same instant as the block.
fn block_that_returned(fixture: &Fixture, delay: Duration) {
    let at = Utc::now() - Duration::days(10);
    let blocked = at + Duration::hours(1);
    let back = blocked + delay;
    dated_task(
        fixture,
        TaskStatus::Done,
        &[
            (at, TaskStatus::InProgress),
            (blocked, TaskStatus::Blocked),
            (back, TaskStatus::Backlog),
            (back + Duration::hours(1), TaskStatus::InProgress),
            (back + Duration::hours(2), TaskStatus::Review),
            (back + Duration::hours(3), TaskStatus::Done),
        ],
    );
}

fn assert_observed_return_can_finish(forecast: &Value, id: &str) {
    let completion = prediction(forecast, id)["completion_probability"]
        .as_f64()
        .unwrap();
    assert!(
        completion >= 0.99,
        "a recorded return must stay possible for a new block: {completion} {}",
        forecast["forecast"]
    );
    let no_return = forecast["forecast"]["outcomes"]["no_return"]
        .as_f64()
        .unwrap();
    assert!(
        no_return <= 0.02,
        "observed returns must not become no-return mass: {no_return}"
    );
    assert!(
        forecast["forecast"]["time_to_drain_hours"]["p50"]
            .as_f64()
            .is_some(),
        "time to drain p50: {}",
        forecast["forecast"]["time_to_drain_hours"]
    );
    assert!(
        forecast["forecast"]["time_to_drain_hours"]["p90"]
            .as_f64()
            .is_some(),
        "time to drain p90: {}",
        forecast["forecast"]["time_to_drain_hours"]
    );
    assert_eq!(forecast["coverage"]["return_tail_unsupported"], 0);
}

#[test]
fn subsecond_and_immediate_returns_stay_drawable() {
    isolated(
        "backlog_outcomes::subsecond_and_immediate_returns_stay_drawable",
        || {
            for delay in [Duration::milliseconds(500), Duration::zero()] {
                let fixture = Fixture::new();
                one_hour_history(&fixture);
                block_that_returned(&fixture, delay);
                let id = backlog_task(&fixture, "Waiting", &["file:wait.rs"], &[], &[]);
                let forecast = forecast(&fixture);
                assert_eq!(forecast["model_version"], 9);
                assert_eq!(
                    forecast["inputs"]["outcomes"]["episodes"],
                    json!({"blocked": 1, "delivered": 1})
                );
                assert_eq!(
                    forecast["inputs"]["outcomes"]["returns"],
                    json!({"waits": 1, "returned": 1, "resolved": 0, "absorbed": 0, "censored": 0})
                );
                assert_observed_return_can_finish(&forecast, &id);
            }

            // Already waiting two hours: the one-hour returns have elapsed,
            // so the draw is the remainder of the day-later return.
            let aged = Fixture::new();
            one_hour_history(&aged);
            block_that_returned(&aged, Duration::hours(1));
            block_that_returned(&aged, Duration::hours(1));
            block_that_returned(&aged, Duration::hours(24));
            dated_task(
                &aged,
                TaskStatus::Blocked,
                &[(Utc::now() - Duration::hours(2), TaskStatus::Blocked)],
            );
            let aged_forecast = forecast(&aged);
            let blocked = aged_forecast["forecast"]["task_predictions"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["status"] == "blocked")
                .unwrap();
            let start = blocked["predicted_start_hours"]["p50"].as_f64().unwrap();
            assert!(
                (start - 22.0).abs() < 0.1,
                "age must drop returns already elapsed: {blocked}"
            );

            // A censored wait clamped to the same instant as the only
            // return leaves an unknown tail at age 0. A fresh block is
            // not past that observation.
            let fresh = Fixture::new();
            one_hour_history(&fresh);
            block_that_returned(&fresh, Duration::zero());
            dated_task(
                &fresh,
                TaskStatus::Blocked,
                &[(Utc::now() + Duration::hours(1), TaskStatus::Blocked)],
            );
            let id = backlog_task(&fresh, "Fresh", &["file:fresh.rs"], &[], &[]);
            let fresh_forecast = forecast(&fresh);
            assert_eq!(
                fresh_forecast["inputs"]["outcomes"]["returns"]["censored"],
                1
            );
            assert_eq!(fresh_forecast["coverage"]["return_tail_unsupported"], 0);
            let completion = prediction(&fresh_forecast, &id)["completion_probability"]
                .as_f64()
                .unwrap();
            assert!(
                completion > 0.6,
                "the same-instant return must still be drawn: {completion}"
            );
        },
    );
}

fn quantile_pair(value: &Value) -> (Option<f64>, Option<f64>) {
    (value["p50"].as_f64(), value["p90"].as_f64())
}

fn metric(value: &Value) -> f64 {
    value
        .as_f64()
        .unwrap_or_else(|| panic!("{value} is not a number"))
}

/// Five one-hour deliveries, one episode, and one queued task. The episode's
/// task ends in `status`, which is outside the forecast cohort.
fn forecast_queued_against(
    label: &str,
    status: TaskStatus,
    steps: &[(DateTime<Utc>, TaskStatus)],
) -> (Value, String) {
    let fixture = Fixture::new();
    one_hour_history(&fixture);
    dated_task(&fixture, status, steps);
    let id = backlog_task(&fixture, label, &["file:queued.rs"], &[], &[]);
    let forecast = forecast(&fixture);
    assert_eq!(forecast["model_version"], 9, "{label}");
    assert_eq!(forecast["inputs"]["history_deliveries"], 5, "{label}");
    assert_eq!(forecast["forecast"]["tasks"], 1, "{label}");
    assert_eq!(
        quantile_pair(&prediction(&forecast, &id)["predicted_start_hours"]),
        (Some(0.0), Some(0.0)),
        "{label} must start at once: {}",
        prediction(&forecast, &id)
    );
    (forecast, id)
}

fn assert_return_wait(forecast: &Value, hours: f64, label: &str) {
    let waits = &forecast["forecast"]["wait_hours"];
    assert_eq!(
        quantile_pair(&waits["return"]),
        (Some(hours), Some(hours)),
        "{label} return wait: {waits}"
    );
    for key in [
        "dependency",
        "lock",
        "reservation",
        "footprint",
        "capacity",
        "resource",
    ] {
        assert_eq!(
            quantile_pair(&waits[key]),
            (Some(0.0), Some(0.0)),
            "{label} also counted the wait as {key}: {waits}"
        );
    }
}

fn assert_hold_without_the_wait(forecast: &Value, parallelism: f64, label: &str) {
    let body = &forecast["forecast"];
    assert_eq!(
        quantile_pair(&body["by_complexity"][0]["work_hours"]),
        (Some(2.0), Some(2.0)),
        "{label} slot hold: {body}"
    );
    assert_eq!(
        quantile_pair(&body["effective_parallelism"]),
        (Some(parallelism), Some(parallelism)),
        "{label} kept a slot during the wait: {body}"
    );
}

#[test]
fn censored_blocks_wait_through_the_horizon_without_a_slot() {
    isolated(
        "backlog_outcomes::censored_blocks_wait_through_the_horizon_without_a_slot",
        || {
            let at = Utc::now() - Duration::days(10);
            let hours = |count: i64| at + Duration::hours(count);
            // Still out of the queue, so the only draw is a censored block.
            // The two-hour hold is the slot; the other 2158 hours are not.
            let (censored, _) = forecast_queued_against(
                "Censored",
                TaskStatus::Someday,
                &[
                    (at, TaskStatus::InProgress),
                    (hours(2), TaskStatus::Someday),
                ],
            );
            assert_eq!(
                censored["inputs"]["outcomes"]["episodes"],
                json!({"blocked": 1})
            );
            assert_eq!(
                censored["inputs"]["outcomes"]["returns"],
                json!({"waits": 1, "returned": 0, "resolved": 0, "absorbed": 0, "censored": 1})
            );
            assert_return_wait(&censored, 2_158.0, "censored");
            assert_hold_without_the_wait(&censored, 0.0, "censored");
            assert_eq!(
                metric(&censored["forecast"]["outcomes"]["blocked_attempts"]),
                1.0
            );
            assert_eq!(metric(&censored["forecast"]["outcomes"]["no_return"]), 1.0);
            assert_eq!(metric(&censored["forecast"]["not_expected_to_drain"]), 1.0);
            assert_eq!(metric(&censored["forecast"]["completion_probability"]), 0.0);
            assert!(censored["forecast"]["time_to_drain_hours"]["p50"].is_null());

            // Resolves 24 hours after the block: one finite return wait,
            // then the task is finished and the rest of the horizon is not.
            let (finite, id) = forecast_queued_against(
                "Finite",
                TaskStatus::Done,
                &[
                    (at, TaskStatus::InProgress),
                    (hours(2), TaskStatus::Blocked),
                    (hours(26), TaskStatus::Done),
                ],
            );
            assert_eq!(finite["inputs"]["outcomes"]["returns"]["resolved"], 1);
            assert_return_wait(&finite, 24.0, "finite");
            assert_hold_without_the_wait(&finite, 0.08, "finite");
            assert_eq!(metric(&finite["forecast"]["outcomes"]["no_return"]), 0.0);
            assert_eq!(metric(&finite["forecast"]["not_expected_to_drain"]), 0.0);
            assert_eq!(metric(&finite["forecast"]["completion_probability"]), 1.0);
            assert_eq!(
                metric(&prediction(&finite, &id)["completion_probability"]),
                1.0
            );
            assert_eq!(
                quantile_pair(&finite["forecast"]["time_to_drain_hours"]),
                (Some(26.0), Some(26.0))
            );

            // Absorption ten hours after the block stops there. It is not
            // a censored wait, and it does not finish the task.
            let (absorbed, _) = forecast_queued_against(
                "Absorbed",
                TaskStatus::Rejected,
                &[
                    (at, TaskStatus::InProgress),
                    (hours(2), TaskStatus::Blocked),
                    (hours(12), TaskStatus::Rejected),
                ],
            );
            assert_eq!(absorbed["inputs"]["outcomes"]["returns"]["absorbed"], 1);
            assert_return_wait(&absorbed, 10.0, "absorbed");
            assert_hold_without_the_wait(&absorbed, 0.0, "absorbed");
            assert_eq!(metric(&absorbed["forecast"]["outcomes"]["no_return"]), 1.0);
            assert_eq!(
                metric(&absorbed["forecast"]["outcomes"]["abandoned_attempts"]),
                0.0
            );
            assert_eq!(metric(&absorbed["forecast"]["not_expected_to_drain"]), 1.0);
            assert_eq!(metric(&absorbed["forecast"]["completion_probability"]), 0.0);
            assert!(absorbed["forecast"]["time_to_drain_hours"]["p50"].is_null());

            // Abandonment is the attempt's end, not time out of the queue.
            let (abandoned, _) = forecast_queued_against(
                "Abandoned",
                TaskStatus::Rejected,
                &[
                    (at, TaskStatus::InProgress),
                    (hours(2), TaskStatus::Rejected),
                ],
            );
            assert_eq!(
                abandoned["inputs"]["outcomes"]["episodes"],
                json!({"abandoned": 1})
            );
            assert_eq!(abandoned["inputs"]["outcomes"]["returns"]["waits"], 0);
            assert_return_wait(&abandoned, 0.0, "abandoned");
            assert_hold_without_the_wait(&abandoned, 0.0, "abandoned");
            assert_eq!(
                metric(&abandoned["forecast"]["outcomes"]["abandoned_attempts"]),
                1.0
            );
            assert_eq!(
                metric(&abandoned["forecast"]["outcomes"]["blocked_attempts"]),
                0.0
            );
            assert_eq!(metric(&abandoned["forecast"]["outcomes"]["no_return"]), 1.0);
            assert_eq!(metric(&abandoned["forecast"]["not_expected_to_drain"]), 1.0);
            assert_eq!(
                metric(&abandoned["forecast"]["completion_probability"]),
                0.0
            );
        },
    );
}

/// Delivery-hour samples, five medium failures of `hold_hours` parked in
/// someday, and two medium backlog tasks. The failures are the only
/// episodes, so every drawn attempt fails for that long. The someday tasks
/// stay out of the cohort.
fn forecast_failed_retries(hold_hours: i64) -> Value {
    let fixture = Fixture::new();
    let at = Utc::now() - Duration::days(10);
    one_hour_history(&fixture);
    for _ in 0..5 {
        dated_task(
            &fixture,
            TaskStatus::Someday,
            &[
                (at, TaskStatus::InProgress),
                (at + Duration::hours(hold_hours), TaskStatus::Backlog),
                (
                    at + Duration::hours(hold_hours) + Duration::minutes(1),
                    TaskStatus::Someday,
                ),
            ],
        );
    }
    backlog_task(&fixture, "First", &["file:cap-a.rs"], &[], &[]);
    backlog_task(&fixture, "Second", &["file:cap-b.rs"], &[], &[]);
    let forecast = forecast(&fixture);
    assert_eq!(forecast["forecast"]["tasks"], 2, "{forecast}");
    assert_eq!(forecast["inputs"]["history_deliveries"], 5, "{forecast}");
    assert_eq!(
        forecast["inputs"]["outcomes"]["episodes"],
        json!({"failed": 5}),
        "the duration samples must not dilute the failed pool: {forecast}"
    );
    forecast
}

#[test]
fn attempt_limit_is_the_mean_of_caps_the_drain_started() {
    isolated(
        "backlog_outcomes::attempt_limit_is_the_mean_of_caps_the_drain_started",
        || {
            // One-hour failures finish 25 attempts on each task inside the
            // horizon: 50 starts, and both tasks hit the cap.
            let short = forecast_failed_retries(1);
            let short_outcomes = &short["forecast"]["outcomes"];
            assert_eq!(metric(&short_outcomes["failed_attempts"]), 50.0);
            assert_eq!(metric(&short_outcomes["blocked_attempts"]), 0.0);
            assert_eq!(metric(&short_outcomes["abandoned_attempts"]), 0.0);
            assert_eq!(metric(&short_outcomes["attempt_limited"]), 2.0);
            assert_eq!(short_outcomes["max_attempts"], 25);

            // 100-hour failures reach the 2160-hour horizon after 22
            // starts. The second task never runs, so neither task starts
            // its 25th attempt even though every script has 25 steps.
            let long = forecast_failed_retries(100);
            let long_outcomes = &long["forecast"]["outcomes"];
            assert_eq!(metric(&long_outcomes["failed_attempts"]), 22.0);
            assert_eq!(metric(&long_outcomes["attempt_limited"]), 0.0);
            assert_eq!(long_outcomes["max_attempts"], 25);
            let mut started: Vec<f64> = long["forecast"]["task_predictions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| metric(&row["started_probability"]))
                .collect();
            started.sort_by(f64::total_cmp);
            assert_eq!(started, vec![0.0, 1.0], "{long}");
        },
    );
}
