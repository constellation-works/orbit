//! A failed job carrying a declared infrastructure signature gets the CI rerun,
//! and a workspace that declares none keeps its repair route.
use super::*;

const SIGNATURE: &str = "ci-download: outcome=failed class=transient";
const DOWNLOAD_504: &str = "\
test\tTests\tcurl: (22) The requested URL returned error: 504
test\tTests\tci-download: attempt=9 curl_exit=22 http='504' seconds=0.400
test\tTests\tci-download: failure=total budget cannot accommodate backoff
test\tTests\tci-download: outcome=failed class=transient attempts=9 total_seconds=57.100 budget_seconds=60.0
test\tTests\t##[error]Process completed with exit code 1.
";

fn failing_download(row: &Path) -> (Fixture, DeliveryHost) {
    let (fx, host, _) = fixture(row);
    let path = fx.forge.join("retry-config.json");
    let mut config: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    config["conclusions"] = json!({"lint": "success"});
    config["repeat"] = json!(true);
    fs::write(path, serde_json::to_vec(&config).unwrap()).unwrap();
    for job in [1000, 1010] {
        fs::write(fx.forge.join(format!("retry-log-{job}")), DOWNLOAD_504).unwrap();
    }
    (fx, host)
}

fn complete_auto_fix(fx: &Fixture, host: &DeliveryHost) -> Res<Value> {
    let mut input = fx.complete_ungated_input();
    input["auto_fix"] = json!(true);
    input["completion"] = json!("review");
    input["max_wait_seconds"] = json!(30);
    input["poll_interval_seconds"] = json!(5);
    complete_with_logical_clock(host, &input)
}

#[test]
fn declared_signature_reruns_without_auto_fix_and_undeclared_repairs() {
    isolated(
        "ci_rerun::infrastructure::declared_signature_reruns_without_auto_fix_and_undeclared_repairs",
        |sandbox| {
            in_fresh_row(sandbox, |row| {
                let (fx, host) = failing_download(row);
                locked(&host.pr_config).ci_infrastructure_signatures = vec![SIGNATURE.to_string()];
                locked(&host.pr_config).ci_infrastructure_retry.max_attempts = 1;
                let error = complete_auto_fix(&fx, &host)
                    .expect_err("a job that keeps failing does not merge");
                assert_eq!(
                    fx.forge_lines("retry-sends"),
                    vec!["run rerun 700 --failed".to_string()]
                );
                assert!(
                    pr_auto_fix_attempts(&host.history(TASK_ID), "42").is_empty(),
                    "no repair attempt: {:?}",
                    host.history(TASK_ID)
                );
                let message = error.to_string();
                assert!(
                    !orbit_types::workflow::is_pr_auto_fix_check_repair(&message),
                    "no repair hand-off: {message}"
                );
                assert!(
                    message.contains("classification=transient attempts=1"),
                    "{message}"
                );
            });
            in_fresh_row(sandbox, |row| {
                let (fx, host) = failing_download(row);
                let error = complete_auto_fix(&fx, &host).expect_err("the repair hand-off");
                assert!(
                    orbit_types::workflow::is_pr_auto_fix_check_repair(&error.to_string()),
                    "{error}"
                );
                assert_eq!(fx.forge_lines("retry-sends"), Vec::<String>::new());
            });
        },
    );
}

#[test]
fn a_quoted_or_scripted_signature_is_not_infrastructure() {
    isolated(
        "ci_rerun::infrastructure::a_quoted_or_scripted_signature_is_not_infrastructure",
        |sandbox| {
            for log in [
                "test\tTests\t##[group]Run grep 'ci-download: outcome=failed class=transient' log\ntest\tTests\tci-download: outcome=failed class=transient\ntest\tTests\t##[endgroup]\ntest\tTests\terror: source failure at src/feature.txt:1:1\n",
                "test\tTests\tassertion failed: stderr was \"ci-download: outcome=failed class=transient\"\ntest\tTests\terror: source failure at src/feature.txt:1:1\n",
            ] {
                in_fresh_row(sandbox, |row| {
                    let (fx, host) = failing_download(row);
                    locked(&host.pr_config).ci_infrastructure_signatures =
                        vec![SIGNATURE.to_string()];
                    fs::write(fx.forge.join("retry-log-1000"), log).unwrap();
                    let error = complete_auto_fix(&fx, &host).expect_err("the repair hand-off");
                    assert!(
                        orbit_types::workflow::is_pr_auto_fix_check_repair(&error.to_string()),
                        "{log}: {error}"
                    );
                    assert_eq!(fx.forge_lines("retry-sends"), Vec::<String>::new(), "{log}");
                });
            }
        },
    );
}

/// The incident left a review delivery red after the first rerun. The
/// existing review activity must keep watching until a later attempt passes.
#[test]
fn review_watch_retries_after_the_outage_clears() {
    isolated(
        "ci_rerun::infrastructure::review_watch_retries_after_the_outage_clears",
        |sandbox| {
            let (fx, host) = failing_download(sandbox);
            locked(&host.pr_config).ci_infrastructure_signatures = vec![SIGNATURE.into()];
            let backoff_seconds = locked(&host.pr_config)
                .ci_infrastructure_retry
                .backoff_seconds;
            let path = fx.forge.join("retry-config.json");
            let mut config: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            config["fail_attempts"] = json!(2);
            fs::write(path, serde_json::to_vec(&config).unwrap()).unwrap();
            let watched = with_completion_clock(|| {
                action(
                    &host,
                    "pr_checks_watch",
                    &json!({
                        "task_id": TASK_ID, "pr_number": "42", "workspace_path": fx.repo,
                        "base": BASE, "base_sync": "local", "max_wait_seconds": 600,
                        "poll_interval_seconds": 5,
                    }),
                )
            })
            .expect("the review watcher retries the infrastructure failure");
            assert_eq!(watched["observation"]["state"], "passed", "{watched}");
            assert_eq!(fx.forge_lines("retry-sends").len(), 2);
            assert_eq!(host.status(TASK_ID), TaskStatus::Review);
            assert!(pr_auto_fix_attempts(&host.history(TASK_ID), "42").is_empty());
            let history = host.history(TASK_ID);
            let reservations = history
                .iter()
                .filter(|entry| entry.event == CI_RERUN_RESERVED_EVENT)
                .collect::<Vec<_>>();
            assert_eq!(reservations.len(), 2);
            assert!(
                (reservations[1].at - reservations[0].at).num_seconds()
                    >= i64::try_from(backoff_seconds).unwrap(),
                "the second attempt honors the configured backoff"
            );
        },
    );
}

#[test]
fn infrastructure_retry_budget_survives_completion_restarts_and_records_hold() {
    isolated(
        "ci_rerun::infrastructure::infrastructure_retry_budget_survives_completion_restarts_and_records_hold",
        |sandbox| {
            let (fx, host) = failing_download(sandbox);
            for job in [1020, 1030] {
                fs::write(fx.forge.join(format!("retry-log-{job}")), DOWNLOAD_504).unwrap();
            }
            {
                let mut config = locked(&host.pr_config);
                config.ci_infrastructure_signatures = vec![SIGNATURE.into()];
                config.ci_infrastructure_retry.max_attempts = 2;
                config.ci_infrastructure_retry.backoff_seconds = 10;
            }
            // Each activity invocation has a fresh in-memory completion state.
            let first = complete_auto_fix(&fx, &host).expect_err("two failures exhaust the budget");
            assert!(
                first
                    .to_string()
                    .contains("ci_infrastructure_retry_exhausted"),
                "{first}"
            );
            assert!(first.to_string().contains("attempts=2"), "{first}");
            let second =
                complete_auto_fix(&fx, &host).expect_err("a restart cannot reset the budget");
            assert!(second.to_string().contains("attempts=2"), "{second}");
            assert_eq!(fx.forge_lines("retry-sends").len(), 2);
            let history = host.history(TASK_ID);
            let held = history
                .iter()
                .filter(|entry| entry.event == "ci_infrastructure_retry_exhausted")
                .collect::<Vec<_>>();
            assert_eq!(held.len(), 1, "the task keeps one distinct, readable hold");
            let note: Value = serde_json::from_str(held[0].note.as_deref().unwrap()).unwrap();
            assert_eq!(note["classification"], "transient");
            assert_eq!(note["attempts"], 2);
            assert!(pr_auto_fix_attempts(&history, "42").is_empty());
        },
    );
}

#[test]
fn a_candidate_sibling_never_uses_the_infrastructure_retry_path() {
    isolated(
        "ci_rerun::infrastructure::a_candidate_sibling_never_uses_the_infrastructure_retry_path",
        |sandbox| {
            let (fx, host) = failing_download(sandbox);
            locked(&host.pr_config).ci_infrastructure_signatures = vec![SIGNATURE.into()];
            let path = fx.forge.join("retry-config.json");
            let mut config: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            config["conclusions"] = json!({"lint":"success", "independent":"failure"});
            fs::write(path, serde_json::to_vec(&config).unwrap()).unwrap();
            fs::write(
                fx.forge.join("retry-log-1002"),
                "independent\tTests\terror: source failure at src/feature.txt:1:1\n",
            )
            .unwrap();
            let watched = with_completion_clock(|| {
                action(
                    &host,
                    "pr_checks_watch",
                    &json!({
                        "task_id": TASK_ID, "pr_number": "42", "workspace_path": fx.repo,
                        "base": BASE, "base_sync": "local", "max_wait_seconds": 600,
                    }),
                )
            })
            .expect("a candidate failure is observed without a retry");
            assert_eq!(
                watched["observation"]["classification"], "candidate",
                "{watched}"
            );
            assert!(fx.forge_lines("retry-sends").is_empty());
            assert!(
                !host
                    .history(TASK_ID)
                    .iter()
                    .any(|entry| entry.event == CI_RERUN_RESERVED_EVENT)
            );
        },
    );
}

#[test]
fn throttled_infrastructure_log_read_defers_then_retries() {
    isolated(
        "ci_rerun::infrastructure::throttled_infrastructure_log_read_defers_then_retries",
        |sandbox| {
            let (fx, host) = failing_download(sandbox);
            locked(&host.pr_config).ci_infrastructure_signatures = vec![SIGNATURE.into()];
            let path = fx.forge.join("retry-config.json");
            let mut config: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            config["repeat"] = json!(false);
            fs::write(path, serde_json::to_vec(&config).unwrap()).unwrap();
            host.script(
                "ci.job_log",
                json!({
                    "fallback_error":"HTTP 403: API rate limit exceeded for user ID 270255973.",
                    "read_throttle":{"retry_after_seconds":10,"principal":"user ID 270255973"}
                }),
            );
            let clock = std::rc::Rc::new(std::cell::Cell::new(1_000_000_u64));
            let read_clock = std::rc::Rc::clone(&clock);
            let wait_clock = std::rc::Rc::clone(&clock);
            let host = std::rc::Rc::new(host);
            let wait_host = std::rc::Rc::clone(&host);
            let waits = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
            let wait_calls = std::rc::Rc::clone(&waits);
            let watched = orbit_engine::test_support::with_prerequisite_clock(
                move || { let now = read_clock.get(); read_clock.set(now + 1); now },
                |_, _| {},
                || orbit_engine::test_support::with_vcs_retry_wait(
                    move |duration| {
                        wait_calls.borrow_mut().push(duration);
                        if locked(&wait_host.scripted).contains_key("ci.job_log") {
                            assert!(!wait_host.history(TASK_ID).iter().any(|entry| entry.event == CI_RERUN_RESERVED_EVENT), "throttled evidence authorizes no reservation");
                        }
                        wait_clock.set(wait_clock.get() + u64::try_from(duration.as_millis()).unwrap());
                        locked(&wait_host.scripted).remove("ci.job_log");
                    },
                    || action(host.as_ref(), "pr_checks_watch", &json!({
                        "task_id": TASK_ID, "pr_number": "42", "workspace_path": fx.repo,
                        "base": BASE, "base_sync": "local", "max_wait_seconds": 600,
                        "poll_interval_seconds":5,
                    })),
                ),
            ).expect("a throttled read defers through completion's existing path");
            assert_eq!(watched["observation"]["state"], "passed", "{watched}");
            assert!(waits.borrow().iter().any(|wait| wait.as_secs() == 10));
            assert_eq!(fx.forge_lines("retry-sends").len(), 1);
            let history = host.history(TASK_ID);
            let deferred = history
                .iter()
                .filter(|entry| entry.event == "pr_auto_fix_read_deferred")
                .collect::<Vec<_>>();
            assert_eq!(deferred.len(), 1);
            let note: Value = serde_json::from_str(deferred[0].note.as_deref().unwrap()).unwrap();
            assert_eq!(note["retry_after_seconds"], 10);
            assert!(pr_auto_fix_attempts(&history, "42").is_empty());
        },
    );
}

#[test]
fn claimed_landing_retries_transient_failures_then_lands() {
    isolated(
        "ci_rerun::infrastructure::claimed_landing_retries_transient_failures_then_lands",
        |sandbox| {
            let (fx, host) = failing_download(sandbox);
            let path = fx.forge.join("retry-config.json");
            let mut config: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            config["fail_attempts"] = json!(2);
            fs::write(path, serde_json::to_vec(&config).unwrap()).unwrap();
            for job in [1000, 1010] {
                fs::write(
                    fx.forge.join(format!("retry-log-{job}")),
                    "test\tTests\terror: TLS handshake timeout\n",
                )
                .unwrap();
            }
            let claimed = claimed(&fx, &host);
            let landed = with_completion_clock(|| {
                execute_deterministic_action(
                    &claimed,
                    "handoff_land",
                    &json!({}),
                    &json!({"handoff_id":"handoff-retry","task_id":TASK_ID,"run_id":RUN_ID,
                    "max_wait_seconds":600,"poll_interval_seconds":5}),
                    false,
                    &HashMap::new(),
                    None,
                )
            });
            assert!(landed.is_ok(), "{landed:?}; {:?}", locked(&claimed.steps));
            assert_eq!(fx.forge_lines("retry-sends").len(), 2);
            assert!(fx.forge.join("retry-merged").exists());
            assert!(pr_auto_fix_attempts(&host.history(TASK_ID), "42").is_empty());
        },
    );
}

#[test]
fn completion_retries_transient_failure_then_returns_to_review() {
    isolated(
        "ci_rerun::infrastructure::completion_retries_transient_failure_then_returns_to_review",
        |sandbox| {
            let (fx, host) = failing_download(sandbox);
            locked(&host.pr_config).ci_infrastructure_signatures = vec![SIGNATURE.into()];
            let path = fx.forge.join("retry-config.json");
            let mut config: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            config["fail_attempts"] = json!(2);
            fs::write(path, serde_json::to_vec(&config).unwrap()).unwrap();
            let mut input = fx.complete_ungated_input();
            input["auto_fix"] = json!(true);
            input["completion"] = json!("review");
            input["max_wait_seconds"] = json!(600);
            input["poll_interval_seconds"] = json!(5);
            let completed = complete_with_logical_clock(&host, &input);
            assert!(completed.is_ok(), "{completed:?}");
            assert_eq!(fx.forge_lines("retry-sends").len(), 2);
            assert_eq!(host.status(TASK_ID), TaskStatus::Review);
            assert!(pr_auto_fix_attempts(&host.history(TASK_ID), "42").is_empty());
        },
    );
}
