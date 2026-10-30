//! Reproduced loss of a planner refusal before completion's deadline hold.
use super::*;
use orbit_types::workflow::DeliveryRefusal;
use std::{cell::Cell, rc::Rc};

fn journal(host: &DeliveryHost) -> Value {
    host.validation_log(TASK_ID, &format!("ci-rerun/{RUN_ID}/diagnostics.json"))
}

fn watch_input(fx: &Fixture, seconds: u64) -> Value {
    let mut input = fx.complete_ungated_input();
    input["auto_fix"] = json!(true);
    input["completion"] = json!("review");
    input["max_wait_seconds"] = json!(seconds);
    input
}

#[test]
fn planner_read_refusal_survives_public_completion_deadline() {
    isolated(
        "ci_rerun::diagnostic::planner_read_refusal_survives_public_completion_deadline",
        |sandbox| {
            for case in ["incomplete", "unattributed", "transient", "provider"] {
                in_fresh_row(sandbox, |row| {
                    let (fx, host, _) = fixture(row);
                    let host = Rc::new(host);
                    let reason = match case {
                        "incomplete" => {
                            host.script("ci.job_log", json!({"source_complete":false}));
                            "the job log source is incomplete"
                        }
                        "unattributed" => {
                            host.script(
                                "ci.job_log",
                                json!({"source_complete":true,"source":"unknown"}),
                            );
                            "the job log source is unknown"
                        }
                        "transient" => {
                            host.script("refuse:ci.job_log", json!({"reason":"temporary log transport unavailable", "transient":true}));
                            "temporary log transport unavailable"
                        }
                        _ => {
                            host.script(
                                "refuse:ci.job_log",
                                json!({"reason":"provider identity lookup refused"}),
                            );
                            "provider identity lookup refused"
                        }
                    };
                    let clock = Rc::new(Cell::new(1_000_000));
                    let now = Rc::clone(&clock);
                    let observed_host = Rc::clone(&host);
                    let result = orbit_engine::test_support::with_prerequisite_clock(
                        move || {
                            if !locked(&observed_host.scripted_inputs).is_empty() {
                                let seen = locked(&observed_host.scripted_inputs);
                                if seen.contains_key("ci-planner-refusal-seen")
                                    || seen.contains_key("ci-planner-log-seen")
                                {
                                    now.set(1_020_000);
                                }
                            }
                            now.get()
                        },
                        |_, _| {},
                        || action(&host, "pr_complete", &watch_input(&fx, 20)),
                    );
                    let error = result.expect_err(case);
                    assert!(
                        matches!(
                            &error,
                            OrbitError::DeliveryRefused {
                                refusal: DeliveryRefusal::CompletionBudgetExpired,
                                ..
                            }
                        ),
                        "{case}: {error}"
                    );
                    assert!(
                        error.to_string().contains(reason),
                        "{case}: original refusal lost: {error}"
                    );
                    let kept = journal(&host);
                    let current = &kept["current"];
                    assert_eq!(current["stage"], "log");
                    assert_eq!(current["pr_number"], 42);
                    assert_eq!(current["head_sha"], fx.candidate);
                    assert_eq!(current["run_id"], 700);
                    assert_eq!(current["job_id"], 1000);
                    assert!(current["reason"].as_str().unwrap().contains(reason));
                    assert_eq!(
                        kept["outcome"]["kind"],
                        "delivery_completion_budget_expired"
                    );
                    assert_eq!(clock.get(), 1_020_000, "the original budget is not renewed");
                    assert!(fx.forge_lines("retry-sends").is_empty());
                    assert!(pr_auto_fix_attempts(&host.history(TASK_ID), "42").is_empty());
                    assert!(fx.merge_requests().is_empty());
                });
            }
        },
    );
}

#[test]
fn planner_refusal_journal_deduplicates_bounds_transitions_and_clears_on_recovery() {
    isolated(
        "ci_rerun::diagnostic::planner_refusal_journal_deduplicates_bounds_transitions_and_clears_on_recovery",
        |sandbox| {
            for case in ["identical", "transitions", "recovered", "head_changed"] {
                in_fresh_row(sandbox, |row| {
                    let (fx, host, _) = fixture(row);
                    let host = Rc::new(host);
                    host.script("refuse:ci.rerun_plan", json!({"reason":format!("CI authentication refused Authorization: Bearer secret-value {}\nraw-provider-output-must-not-survive", "é".repeat(1500))}));
                    let clock = Rc::new(Cell::new(1_000_000_u64));
                    let now = Rc::clone(&clock);
                    let waited = Rc::clone(&clock);
                    let polls = Rc::new(Cell::new(0));
                    let wait_polls = Rc::clone(&polls);
                    let wait_host = Rc::clone(&host);
                    let forge = fx.forge.clone();
                    let budget = if case == "transitions" { 100 } else { 20 };
                    let result = orbit_engine::test_support::with_prerequisite_clock(
                        move || now.get(),
                        |_, _| {},
                        || {
                            orbit_engine::test_support::with_vcs_retry_wait(
                                move |duration| {
                                    waited.set(
                                        waited.get() + u64::try_from(duration.as_millis()).unwrap(),
                                    );
                                    wait_polls.set(wait_polls.get() + 1);
                                    if case == "transitions" {
                                        wait_host.script("refuse:ci.rerun_plan", json!({"reason":format!("CI policy refusal {}", wait_polls.get())}));
                                    } else if case == "recovered" {
                                        locked(&wait_host.scripted).remove("refuse:ci.rerun_plan");
                                    } else if case == "head_changed" {
                                        let path = forge.join("retry-config.json");
                                        let mut config: Value =
                                            serde_json::from_slice(&fs::read(&path).unwrap())
                                                .unwrap();
                                        config["head"] = json!("f".repeat(40));
                                        fs::write(path, serde_json::to_vec(&config).unwrap())
                                            .unwrap();
                                    }
                                },
                                || action(&host, "pr_complete", &watch_input(&fx, budget)),
                            )
                        },
                    );
                    let kept = journal(&host);
                    let text = kept.to_string();
                    assert!(!text.contains("secret-value"), "{text}");
                    assert!(!text.contains("raw-provider-output-must-not-survive"));
                    let observations = kept["observations"].as_array().unwrap();
                    assert!(observations.len() <= 16);
                    assert!(
                        !host
                            .history(TASK_ID)
                            .iter()
                            .any(|entry| entry.event == "ci_retry_wait")
                    );
                    match case {
                        "identical" => {
                            assert!(result.is_err());
                            assert_eq!(
                                observations.len(),
                                1,
                                "unchanged polls must not grow evidence"
                            );
                            assert!(kept["current"]["reason"].as_str().unwrap().len() <= 1024);
                            assert!(polls.get() >= 3);
                            assert_eq!(clock.get(), 1_020_000);
                        }
                        "transitions" => {
                            assert!(result.is_err());
                            assert_eq!(observations.len(), 16);
                            assert!(kept["omitted_observations"].as_u64().unwrap() > 0);
                            assert_ne!(observations[0]["reason"], observations[15]["reason"]);
                        }
                        "recovered" => {
                            assert!(result.is_ok(), "{result:?}");
                            assert!(kept["current"].is_null());
                            assert_eq!(kept["outcome"]["kind"], "completed");
                            assert_eq!(
                                fx.forge_lines("retry-sends"),
                                vec!["run rerun 700 --failed"]
                            );
                        }
                        "head_changed" => {
                            let error = result.unwrap_err();
                            assert!(
                                !error.to_string().contains("CI retry diagnostic:"),
                                "stale-head refusal must not be current: {error}"
                            );
                            assert!(kept["current"].is_null());
                            assert_eq!(kept["observed_head"], "f".repeat(40));
                            assert_eq!(observations[0]["head_sha"], fx.candidate);
                        }
                        _ => unreachable!(),
                    }
                    if case != "recovered" {
                        assert!(fx.forge_lines("retry-sends").is_empty());
                    }
                    assert!(pr_auto_fix_attempts(&host.history(TASK_ID), "42").is_empty());
                });
            }
        },
    );
}
