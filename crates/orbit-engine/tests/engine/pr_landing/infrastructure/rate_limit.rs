//! [ORB-25864] GitHub rate-limiting the automation account's prerequisite
//! read failed `pr_complete` and blocked seven complete candidates in one
//! drain. Completion now waits for the reset within its deadline and
//! completes, and a repeated snapshot reuses the repository identity.
//! [ORB-25898] The same wait covers the reads after the snapshot.
//! [ORB-25918] So does a throttled fallback behind an empty prerequisite log.
use super::*;
use std::{cell::Cell, cell::RefCell, rc::Rc};

/// Run `pr_complete` on a logical clock starting at 1000 s, returning its
/// result and every fixed wait it took, in milliseconds.
fn complete_on_logical_clock(
    host: &DeliveryHost,
    input: &Value,
) -> (Result<Value, orbit_common::OrbitError>, Vec<u64>) {
    let clock = Rc::new(Cell::new(1_000_000_u64));
    let waits = Rc::new(RefCell::new(Vec::new()));
    let (now, waited, recorded) = (Rc::clone(&clock), clock, Rc::clone(&waits));
    let result = orbit_engine::test_support::with_prerequisite_clock(
        move || {
            let current = now.get();
            now.set(current + 1);
            current
        },
        |_, _| {},
        || {
            orbit_engine::test_support::with_vcs_retry_wait(
                move |duration| {
                    let millis = u64::try_from(duration.as_millis()).unwrap();
                    waited.set(waited.get() + millis);
                    recorded.borrow_mut().push(millis);
                },
                || action(host, "pr_complete", input),
            )
        },
    );
    (result, waits.take())
}

fn calls(fx: &Fixture, call: &str) -> usize {
    fs::read_to_string(fx.forge.join("calls"))
        .unwrap()
        .lines()
        .filter(|line| *line == call)
        .count()
}

#[test]
fn a_rate_limited_prerequisite_read_waits_for_the_reset_and_completes() {
    isolated(
        "infrastructure::rate_limit::a_rate_limited_prerequisite_read_waits_for_the_reset_and_completes",
        |sandbox| {
            in_fresh_row(sandbox, |row| {
                let fx = fixture_with_workflow(row);
                let host = DeliveryHost::new(&fx.repo, TaskStatus::Review);
                action(
                    &host,
                    "pr_open",
                    &fx.open_input(&fx.candidate, &fx.base_sha),
                )
                .unwrap();
                setup(&fx, "positive");
                // The first repository read is rate-limited; the core quota
                // resets at 1300 s on the logical clock, inside the budget.
                fs::write(fx.forge.join("infra-rate-limited"), "1").unwrap();
                fs::write(
                    fx.forge.join("infra-rate-limit"),
                    json!({"resources":{
                        "core":{"limit":5000,"remaining":0,"reset":1300},
                        "graphql":{"limit":5000,"remaining":4000,"reset":1900}
                    }})
                    .to_string(),
                )
                .unwrap();
                let mut input = fx.complete_ungated_input();
                input["auto_fix"] = json!(true);
                input["completion"] = json!("review");
                input["max_wait_seconds"] = json!(600);
                let (result, waits) = complete_on_logical_clock(&host, &input);
                let output = result.expect("completion waits for the reset and completes");
                assert_eq!(output["merge"]["head_sha"], json!(fx.candidate), "{output}");
                assert_eq!(
                    host.status(TASK_ID),
                    TaskStatus::Review,
                    "the task is not blocked"
                );
                assert!(
                    waits.iter().any(|wait| (299_000..=300_000).contains(wait)),
                    "completion waited until the provider's reset: {waits:?}"
                );
                assert_eq!(calls(&fx, "api rate_limit"), 1, "the reset is read once");
                assert_eq!(
                    calls(&fx, "run rerun"),
                    1,
                    "recovery proceeds after the wait"
                );
                // The throttled read and its one repeat; every later snapshot
                // of this completion reuses the repository identity.
                assert_eq!(
                    calls(&fx, "api repos/{owner}/{repo}"),
                    2,
                    "{}",
                    fs::read_to_string(fx.forge.join("calls")).unwrap()
                );
            });
        },
    );
}

/// Throttle the first workflow-file read, which follows a readable snapshot,
/// for `times` reads, with the core quota resetting at `reset` seconds on the
/// logical clock.
fn throttle_workflow_file_reads(fx: &Fixture, times: u32, reset: u64) {
    fs::write(
        fx.forge.join("infra-contents-rate-limited"),
        times.to_string(),
    )
    .unwrap();
    fs::write(
        fx.forge.join("infra-rate-limit"),
        json!({"resources":{
            "core":{"limit":5000,"remaining":0,"reset":reset},
            "graphql":{"limit":5000,"remaining":4000,"reset":reset}
        }})
        .to_string(),
    )
    .unwrap();
}

fn complete_review_input(fx: &Fixture) -> Value {
    let mut input = fx.complete_ungated_input();
    input["auto_fix"] = json!(true);
    input["completion"] = json!("review");
    input["max_wait_seconds"] = json!(600);
    input
}

#[test]
fn a_rate_limited_workflow_file_read_after_the_snapshot_waits_before_the_rerun() {
    isolated(
        "infrastructure::rate_limit::a_rate_limited_workflow_file_read_after_the_snapshot_waits_before_the_rerun",
        |sandbox| {
            in_fresh_row(sandbox, |row| {
                let fx = fixture_with_workflow(row);
                let host = DeliveryHost::new(&fx.repo, TaskStatus::Review);
                action(
                    &host,
                    "pr_open",
                    &fx.open_input(&fx.candidate, &fx.base_sha),
                )
                .unwrap();
                setup(&fx, "positive");
                throttle_workflow_file_reads(&fx, 1, 1300);
                let (result, waits) = complete_on_logical_clock(&host, &complete_review_input(&fx));
                let output = result.expect("completion waits for the reset and completes");
                assert_eq!(output["merge"]["head_sha"], json!(fx.candidate), "{output}");
                assert_eq!(host.status(TASK_ID), TaskStatus::Review);
                assert!(
                    waits.iter().any(|wait| (299_000..=300_000).contains(wait)),
                    "completion waited until the provider's reset: {waits:?}"
                );
                assert_eq!(calls(&fx, "api rate_limit"), 1, "the reset is read once");
                assert_eq!(
                    fs::read_to_string(fx.forge.join("infra-contents-rate-limited"))
                        .unwrap()
                        .trim(),
                    "0",
                    "the throttled read was repeated after the wait"
                );
                assert_eq!(
                    calls(&fx, "run rerun"),
                    1,
                    "the rerun is admitted once, after the evidence was read again"
                );
            });
        },
    );
}

#[test]
fn a_workflow_file_read_reset_past_the_budget_keeps_the_provider_diagnostic() {
    isolated(
        "infrastructure::rate_limit::a_workflow_file_read_reset_past_the_budget_keeps_the_provider_diagnostic",
        |sandbox| {
            in_fresh_row(sandbox, |row| {
                let fx = fixture_with_workflow(row);
                let host = DeliveryHost::new(&fx.repo, TaskStatus::Review);
                action(
                    &host,
                    "pr_open",
                    &fx.open_input(&fx.candidate, &fx.base_sha),
                )
                .unwrap();
                setup(&fx, "positive");
                // Reset an hour out on a 600 s budget; every read stays throttled.
                throttle_workflow_file_reads(&fx, 9, 1_000 + 3_600);
                let (result, waits) = complete_on_logical_clock(&host, &complete_review_input(&fx));
                let error = result.expect_err("a reset past the deadline is not waited for");
                assert!(
                    error.to_string().contains("API rate limit exceeded"),
                    "the provider diagnostic is kept: {error}"
                );
                assert!(
                    waits.iter().all(|wait| *wait < 299_000),
                    "no wait toward the out-of-budget reset: {waits:?}"
                );
                assert_eq!(calls(&fx, "run rerun"), 0, "no rerun was sent");
                assert_eq!(
                    fs::read_to_string(fx.forge.join("infra-contents-rate-limited"))
                        .unwrap()
                        .trim(),
                    "8",
                    "the throttled read was not repeated"
                );
            });
        },
    );
}

/// The provider calls this completion made, oldest first.
fn call_log(fx: &Fixture) -> Vec<String> {
    fs::read_to_string(fx.forge.join("calls"))
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect()
}

/// Serve the prerequisite log empty for the next `times` reads, and throttle the
/// fallback's run-metadata read for the next `times` reads, with the core quota
/// resetting at `reset` seconds on the logical clock. [ORB-25918]
fn empty_log_and_throttle_fallback_metadata(fx: &Fixture, times: u32, reset: u64) {
    fs::write(fx.forge.join("infra-log-empty"), times.to_string()).unwrap();
    fs::write(fx.forge.join("infra-run-rate-limited"), times.to_string()).unwrap();
    fs::write(
        fx.forge.join("infra-rate-limit"),
        json!({"resources":{
            "core":{"limit":5000,"remaining":0,"reset":reset},
            "graphql":{"limit":5000,"remaining":4000,"reset":reset}
        }})
        .to_string(),
    )
    .unwrap();
}

/// [ORB-25918] An empty prerequisite log whose fallback metadata read is
/// throttled is an unreadable log with a known reset. The completion waits for
/// that reset inside its deadline, reads the whole evidence again, and only then
/// sends the rerun.
#[test]
fn a_throttled_prerequisite_log_fallback_waits_for_the_reset_and_rereads_the_log() {
    isolated(
        "infrastructure::rate_limit::a_throttled_prerequisite_log_fallback_waits_for_the_reset_and_rereads_the_log",
        |sandbox| {
            in_fresh_row(sandbox, |row| {
                let fx = fixture_with_workflow(row);
                let host = DeliveryHost::new(&fx.repo, TaskStatus::Review);
                action(
                    &host,
                    "pr_open",
                    &fx.open_input(&fx.candidate, &fx.base_sha),
                )
                .unwrap();
                setup(&fx, "positive");
                // One empty primary read, then one throttled fallback metadata
                // read; the core quota resets at 1300 s, inside the budget.
                empty_log_and_throttle_fallback_metadata(&fx, 1, 1300);
                let (result, waits) = complete_on_logical_clock(&host, &complete_review_input(&fx));
                let output = result.expect("completion waits for the reset and completes");
                assert_eq!(output["merge"]["head_sha"], json!(fx.candidate), "{output}");
                assert_eq!(host.status(TASK_ID), TaskStatus::Review);
                assert!(
                    waits.iter().any(|wait| (299_000..=300_000).contains(wait)),
                    "completion waited until the provider's reset: {waits:?}"
                );
                assert_eq!(calls(&fx, "api rate_limit"), 1, "the reset is read once");
                let order = call_log(&fx);
                let reset = order
                    .iter()
                    .position(|call| call == "api rate_limit")
                    .expect("the reset is read");
                let rerun = order
                    .iter()
                    .position(|call| call == "run rerun")
                    .expect("the rerun is sent");
                assert!(
                    order[reset..rerun].iter().any(|call| call == "run view"),
                    "the prerequisite log is read again after the reset and before the rerun: {order:?}"
                );
                assert_eq!(calls(&fx, "run rerun"), 1, "the rerun is admitted once");
            });
        },
    );
}

/// [ORB-25918] A throttled prerequisite-log fallback whose reset falls outside
/// the completion deadline keeps GitHub's diagnostic and sends no rerun.
#[test]
fn a_throttled_prerequisite_log_reset_past_the_budget_keeps_the_provider_diagnostic() {
    isolated(
        "infrastructure::rate_limit::a_throttled_prerequisite_log_reset_past_the_budget_keeps_the_provider_diagnostic",
        |sandbox| {
            in_fresh_row(sandbox, |row| {
                let fx = fixture_with_workflow(row);
                let host = DeliveryHost::new(&fx.repo, TaskStatus::Review);
                action(
                    &host,
                    "pr_open",
                    &fx.open_input(&fx.candidate, &fx.base_sha),
                )
                .unwrap();
                setup(&fx, "positive");
                // Reset an hour out on a 600 s budget; every read stays throttled.
                empty_log_and_throttle_fallback_metadata(&fx, 9, 1_000 + 3_600);
                let (result, waits) = complete_on_logical_clock(&host, &complete_review_input(&fx));
                let error = result.expect_err("a reset past the deadline is not waited for");
                assert!(
                    error.to_string().contains("API rate limit exceeded"),
                    "the provider diagnostic is kept: {error}"
                );
                assert!(
                    waits.iter().all(|wait| *wait < 299_000),
                    "no wait toward the out-of-budget reset: {waits:?}"
                );
                assert_eq!(calls(&fx, "run rerun"), 0, "no rerun was sent");
                assert_eq!(
                    fs::read_to_string(fx.forge.join("infra-run-rate-limited"))
                        .unwrap()
                        .trim(),
                    "8",
                    "the throttled fallback read was not repeated"
                );
            });
        },
    );
}
