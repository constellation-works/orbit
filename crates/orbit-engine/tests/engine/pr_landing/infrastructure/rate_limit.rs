//! [ORB-25864] GitHub rate-limiting the automation account's prerequisite
//! read failed `pr_complete` and blocked seven complete candidates in one
//! drain. Completion now waits for the reset within its deadline and
//! completes, and a repeated snapshot reuses the repository identity.
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
