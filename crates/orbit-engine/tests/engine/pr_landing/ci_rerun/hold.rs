//! [ORB-20771] CI rerun planner holds end: a candidate sibling repairs, and
//! every hold is recorded and expires into the repair path.
use super::*;

fn patch_config(fx: &Fixture, edit: impl FnOnce(&mut Value)) {
    let path = fx.forge.join("retry-config.json");
    let mut config: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    edit(&mut config);
    fs::write(path, serde_json::to_vec(&config).unwrap()).unwrap();
}

fn complete_auto_fix(host: &DeliveryHost, fx: &Fixture, max_wait: u64, poll: u64) -> Res<Value> {
    let mut input = fx.complete_ungated_input();
    input["auto_fix"] = json!(true);
    input["completion"] = json!("review");
    input["max_wait_seconds"] = json!(max_wait);
    input["poll_interval_seconds"] = json!(poll);
    complete_with_logical_clock(host, &input)
}

fn notes(host: &DeliveryHost, event: &str) -> Vec<Value> {
    host.history(TASK_ID)
        .iter()
        .filter(|entry| entry.event == event)
        .filter_map(|entry| serde_json::from_str(entry.note.as_deref()?).ok())
        .collect()
}

/// The #7432 shape: the aggregate check only reports a dependency record, and
/// a guardrail sibling fails with a log that cites no changed file.
#[test]
fn candidate_guardrail_beside_dependency_record_reaches_auto_fix() {
    isolated(
        "ci_rerun::hold::candidate_guardrail_beside_dependency_record_reaches_auto_fix",
        |sandbox| {
            let (fx, host, _) = fixture(sandbox);
            patch_config(&fx, |config| {
                config["conclusions"] = json!({"test": "success", "independent": "failure"});
            });
            fs::write(
                fx.forge.join("retry-log-1002"),
                "independent\tTests\terror: bundled auto-task body history is stale; \
                 update the body history beside the edited auto-task\n",
            )
            .unwrap();
            let error = complete_auto_fix(&host, &fx, 30, 5)
                .expect_err("the guardrail failure is handed to a repair");
            assert!(
                orbit_types::workflow::is_pr_auto_fix_check_repair(&error.to_string()),
                "a candidate sibling must not wait on the dependency record: {error}"
            );
            assert!(fx.forge_lines("retry-sends").is_empty());
            assert_eq!(pr_auto_fix_attempts(&host.history(TASK_ID), "42").len(), 1);
        },
    );
}

/// A planner hold that never resolves is recorded with its reason and, past
/// its deadline, goes to the repair path instead of timing out silently.
#[test]
fn dependency_only_hold_is_recorded_and_expires_into_repair() {
    isolated(
        "ci_rerun::hold::dependency_only_hold_is_recorded_and_expires_into_repair",
        |sandbox| {
            let (fx, host, _) = fixture(sandbox);
            patch_config(&fx, |config| {
                config["conclusions"] = json!({"test": "success"});
            });
            let reads = |fx: &Fixture| {
                fx.forge_calls()
                    .iter()
                    .filter(|call| call.as_str() == "pr view")
                    .count()
            };
            let before = reads(&fx);
            // The hold deadline (15 minutes) is far inside this budget.
            let error = complete_auto_fix(&host, &fx, 6 * 60 * 60, 300)
                .expect_err("the red pull request does not merge");
            let message = error.to_string();
            assert!(
                !message.contains("timed out after"),
                "an expired hold proceeds to repair or a reasoned stop: {message}"
            );
            let held = notes(&host, "pr_auto_fix_ci_rerun_hold");
            assert_eq!(held.len(), 1, "{:?}", host.history(TASK_ID));
            assert_eq!(held[0]["head"], fx.candidate);
            assert!(
                held[0]["reason"]
                    .as_str()
                    .is_some_and(|reason| reason.contains("dependency-only evidence")),
                "{held:?}"
            );
            assert_eq!(notes(&host, "pr_auto_fix_ci_rerun_hold_expired"), held);
            assert!(
                reads(&fx) - before < 12,
                "the hold expires at its own deadline, not the 6h budget"
            );
        },
    );
}

/// A throttled failed-job log read defers the next observation without
/// restarting the dependency-only planner hold's original clock.
#[test]
fn dependency_only_hold_expires_from_its_original_start_after_throttled_log_read() {
    isolated(
        "ci_rerun::hold::dependency_only_hold_expires_from_its_original_start_after_throttled_log_read",
        |sandbox| {
            const START_MS: u64 = 1_000_000;
            let (fx, host, _) = fixture(sandbox);
            patch_config(&fx, |config| {
                config["conclusions"] = json!({"test": "success"});
            });
            let mut input = fx.complete_ungated_input();
            input["auto_fix"] = json!(true);
            input["completion"] = json!("review");
            input["max_wait_seconds"] = json!(6 * 60 * 60);
            input["poll_interval_seconds"] = json!(10 * 60);

            let host = std::rc::Rc::new(host);
            let clock = std::rc::Rc::new(std::cell::Cell::new(START_MS));
            let read_clock = std::rc::Rc::clone(&clock);
            let wait_clock = std::rc::Rc::clone(&clock);
            let wait_count = std::rc::Rc::new(std::cell::Cell::new(0_u32));
            let wait_calls = std::rc::Rc::clone(&wait_count);
            let wait_host = std::rc::Rc::clone(&host);
            let result = orbit_engine::test_support::with_prerequisite_clock(
                move || {
                    let current = read_clock.get();
                    read_clock.set(current + 1);
                    current
                },
                |_, _| {},
                || {
                    orbit_engine::test_support::with_vcs_retry_wait(
                        move |duration| {
                            let millis = u64::try_from(duration.as_millis()).unwrap();
                            wait_clock.set(wait_clock.get().saturating_add(millis));
                            match wait_calls.get() {
                                0 => {
                                    // The first poll has already recorded its
                                    // dependency-only hold. Throttle the next
                                    // planner log read and resume at 1,000s,
                                    // beyond the hold's original 900s limit.
                                    wait_host.script(
                                        "ci.job_log",
                                        json!({
                                            "fallback_error": "HTTP 403: API rate limit exceeded for user ID 270255973.",
                                            "read_throttle": {
                                                "retry_after_seconds": 400,
                                                "principal": "user ID 270255973"
                                            }
                                        }),
                                    );
                                }
                                1 => {
                                    // The deferred log read continues in a new
                                    // poll, so let it observe the real fixture.
                                    locked(&wait_host.scripted).remove("ci.job_log");
                                }
                                _ => {}
                            }
                            wait_calls.set(wait_calls.get() + 1);
                        },
                        || action(&host, "pr_complete", &input),
                    )
                },
            );

            let error = result
                .expect_err("an expired dependency-only hold reaches ordinary repair")
                .to_string();
            assert!(
                orbit_types::workflow::is_pr_auto_fix_check_repair(&error),
                "the expired hold takes the ordinary repair path: {error}"
            );
            assert!(
                !error.contains("timed out after"),
                "the original hold deadline expires before completion: {error}"
            );
            assert_eq!(
                wait_count.get(),
                2,
                "one poll wait and one throttle wait; error={error}; history={:?}",
                host.history(TASK_ID)
            );
            assert!(
                locked(&host.scripted_inputs).contains_key("ci-planner-log-seen"),
                "the planner observed the throttled failed-job log"
            );
            assert!(
                clock.get() < START_MS + 1_500_000,
                "the later Waiting observation uses the original 900s clock, got {}ms",
                clock.get() - START_MS
            );
            let deferred = notes(&host, "pr_auto_fix_read_deferred");
            assert_eq!(deferred.len(), 1, "{deferred:?}");
            assert_eq!(deferred[0]["retry_after_seconds"], json!(400));
            let held = notes(&host, "pr_auto_fix_ci_rerun_hold");
            assert_eq!(held.len(), 1, "{:?}", host.history(TASK_ID));
            assert!(
                held[0]["reason"]
                    .as_str()
                    .is_some_and(|reason| reason.contains("dependency-only evidence")),
                "{held:?}"
            );
            assert_eq!(notes(&host, "pr_auto_fix_ci_rerun_hold_expired"), held);
        },
    );
}
