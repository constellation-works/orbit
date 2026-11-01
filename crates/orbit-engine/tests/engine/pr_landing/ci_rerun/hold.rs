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
