//! [ORB-25864] GitHub rate-limiting the automation account's prerequisite
//! read failed `pr_complete` and blocked seven complete candidates in one
//! drain. Completion now waits for the reset within its deadline and
//! completes, and a repeated snapshot reuses the repository identity.
//! [ORB-25898] The same wait covers the reads after the snapshot.
//! [ORB-25918] So does a throttled fallback behind an empty prerequisite log.
//! [ORB-25917] A snapshot that spent its waits ends the pass instead of
//! being read again with a fresh count.
//! [ORB-25938] With the host guard on, completion's reset lookup shares the
//! admission probe's refresh interval.
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

/// Allow `reset` seconds on the logical clock for the core quota to reset.
fn name_core_reset(fx: &Fixture, reset: u64) {
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

/// [ORB-25917] A snapshot read that stays throttled spends the snapshot's three
/// waits and ends the pass with GitHub's diagnostic. The enclosing evidence
/// pass does not read the snapshot again with a fresh count, so there are four
/// snapshot reads, three reset lookups and no rerun.
#[test]
fn a_persistently_throttled_snapshot_stops_after_its_three_waits() {
    isolated(
        "infrastructure::rate_limit::a_persistently_throttled_snapshot_stops_after_its_three_waits",
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
                name_core_reset(&fx, 1300);
                fs::write(fx.forge.join("infra-pulls-rate-limited"), "99").unwrap();
                let (result, waits) = complete_on_logical_clock(&host, &complete_review_input(&fx));
                let error = result.expect_err("a spent snapshot keeps the provider diagnostic");
                assert!(
                    error.to_string().contains("API rate limit exceeded"),
                    "the provider diagnostic is kept: {error}"
                );
                assert_eq!(waits.len(), 3, "the snapshot's three waits only: {waits:?}");
                assert_eq!(
                    calls(&fx, "api repos/{owner}/{repo}/pulls/42"),
                    4,
                    "no snapshot read beyond the first and its three repeats"
                );
                assert_eq!(calls(&fx, "api rate_limit"), 3, "one reset lookup per wait");
                assert_eq!(calls(&fx, "run rerun"), 0, "no rerun was sent");
            });
        },
    );
}

/// [ORB-25917] The same bound holds when observing a reserved rerun: a
/// snapshot that stays throttled from the rerun on ends the observation after
/// its three waits and sends no further mutation.
#[test]
fn a_persistently_throttled_observation_snapshot_stops_after_its_three_waits() {
    isolated(
        "infrastructure::rate_limit::a_persistently_throttled_observation_snapshot_stops_after_its_three_waits",
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
                name_core_reset(&fx, 1300);
                fs::write(fx.forge.join("infra-pulls-rate-limited-after-rerun"), "99").unwrap();
                let (result, waits) = complete_on_logical_clock(&host, &complete_review_input(&fx));
                let error = result.expect_err("a spent snapshot keeps the provider diagnostic");
                assert!(
                    error.to_string().contains("API rate limit exceeded"),
                    "the provider diagnostic is kept: {error}"
                );
                assert_eq!(waits.len(), 3, "the snapshot's three waits only: {waits:?}");
                assert_eq!(calls(&fx, "api rate_limit"), 3, "one reset lookup per wait");
                assert_eq!(
                    calls(&fx, "run rerun"),
                    1,
                    "observing the reserved rerun sends no additional mutation"
                );
            });
        },
    );
}

/// [ORB-25938] A due admission probe finds quota left; the protected read is
/// then throttled with no reset or retry header. Completion's reset lookup
/// answers from the shared record instead of calling `api rate_limit` again
/// inside the interval, and still waits for the reset the record holds.
#[test]
fn a_headerless_throttle_after_a_due_probe_reads_the_reset_from_the_shared_record() {
    use orbit_tools::github_quota::{self, GithubQuotaPolicy, GithubQuotaStore};

    isolated(
        "infrastructure::rate_limit::a_headerless_throttle_after_a_due_probe_reads_the_reset_from_the_shared_record",
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
                // The guard's record uses real epoch seconds, so the probe's
                // answer and the logical clock are anchored to the real now.
                let real_ms = u64::try_from(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_millis(),
                )
                .unwrap();
                let reset = real_ms / 1_000 + 300;
                fs::write(fx.forge.join("infra-rate-limited"), "9").unwrap();
                fs::write(
                    fx.forge.join("infra-rate-limit"),
                    json!({"resources":{
                        "core":{"limit":5000,"remaining":4000,"reset":reset},
                        "graphql":{"limit":5000,"remaining":4000,"reset":reset}
                    }})
                    .to_string(),
                )
                .unwrap();
                fs::write(fx.forge.join("calls"), "").unwrap();
                let records = tempfile::TempDir::new().unwrap();
                let store = GithubQuotaStore::new(records.path().to_path_buf());
                let clock = Rc::new(Cell::new(real_ms));
                let waits = Rc::new(RefCell::new(Vec::new()));
                let (now, recorded) = (Rc::clone(&clock), Rc::clone(&waits));
                let input = complete_review_input(&fx);
                let result = orbit_engine::test_support::with_prerequisite_clock(
                    move || {
                        let current = now.get();
                        now.set(current + 1);
                        current
                    },
                    |_, _| {},
                    || {
                        github_quota::with_scoped_guard(store, GithubQuotaPolicy::default(), || {
                            orbit_engine::test_support::with_vcs_retry_wait(
                                move |duration| {
                                    recorded
                                        .borrow_mut()
                                        .push(u64::try_from(duration.as_millis()).unwrap());
                                },
                                || action(&host, "pr_complete", &input),
                            )
                        })
                    },
                );
                let error = result.expect_err("the quota stays held for the real clock");
                assert!(
                    error.to_string().contains("refused locally"),
                    "completion keeps the typed local refusal: {error}"
                );
                assert!(
                    waits
                        .borrow()
                        .iter()
                        .any(|wait| (290_000..=300_000).contains(wait)),
                    "completion waited toward the recorded reset: {:?}",
                    waits.borrow()
                );
                assert_eq!(
                    calls(&fx, "api rate_limit"),
                    1,
                    "the lookup shares the admission probe's interval: {}",
                    fs::read_to_string(fx.forge.join("calls")).unwrap()
                );
                assert_eq!(
                    calls(&fx, "api repos/{owner}/{repo}"),
                    1,
                    "only the first read reached the provider; the hold refuses the rest"
                );
            });
        },
    );
}
