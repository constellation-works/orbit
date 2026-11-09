//! Cancelled and timed-out jobs rerun from the conclusion when the log is unreadable.
use super::*;

const AWAIT_CANCELLED: &str = "\
lint\tWait\tci-await: test concluded cancelled: https://github.com/orbit/test/actions/runs/700
lint\tWait\tci-await-api: listings=1 not_modified=0 paginated=false
";

/// A waiter that timed out itself. No awaited job concluded `cancelled` or
/// `timed_out`, so this log is the set's only root.
const AWAIT_TIMEOUT: &str = "\
lint\tWait\tci-await: timed out after 2400 s waiting for test (in_progress)
lint\tWait\tci-await-api: listings=1 not_modified=0 paginated=false
";

fn patch_config(fx: &Fixture, edit: impl FnOnce(&mut Value)) {
    let path = fx.forge.join("retry-config.json");
    let mut config: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    edit(&mut config);
    fs::write(path, serde_json::to_vec(&config).unwrap()).unwrap();
}

fn complete_auto_fix(fx: &Fixture, host: &DeliveryHost) -> Res<Value> {
    let mut input = fx.complete_ungated_input();
    input["auto_fix"] = json!(true);
    input["completion"] = json!("review");
    input["max_wait_seconds"] = json!(30);
    input["poll_interval_seconds"] = json!(5);
    complete_with_logical_clock(host, &input)
}

fn assert_no_auto_fix(host: &DeliveryHost) {
    assert!(
        pr_auto_fix_attempts(&host.history(TASK_ID), "42").is_empty(),
        "a transient cancellation spends no auto-fix attempt: {:?}",
        host.history(TASK_ID)
    );
}

fn rerun_classifications(host: &DeliveryHost) -> Vec<String> {
    host.history(TASK_ID)
        .iter()
        .find(|entry| entry.event == "ci_check_rerun")
        .and_then(|entry| entry.note.as_deref())
        .and_then(|note| serde_json::from_str::<Value>(note).ok())
        .and_then(|note| note["jobs"].as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|job| job["classification"].as_str().map(str::to_string))
        .collect()
}

#[test]
fn cancelled_dependency_with_unreadable_log_reruns_without_auto_fix() {
    isolated(
        "ci_rerun::cancelled::cancelled_dependency_with_unreadable_log_reruns_without_auto_fix",
        |sandbox| {
            let (fx, host, _) = fixture(sandbox);
            patch_config(&fx, |config| {
                config["conclusions"] = json!({"test": "cancelled"});
                config["log_404_jobs"] = json!([1000]);
            });
            fs::write(fx.forge.join("retry-log-1001"), AWAIT_CANCELLED).unwrap();
            let error = complete_auto_fix(&fx, &host).expect_err("the stop stays blocked");
            assert_eq!(
                fx.forge_lines("retry-sends"),
                vec!["run rerun 700 --failed".to_string()]
            );
            assert_no_auto_fix(&host);
            assert_eq!(
                rerun_classifications(&host),
                vec!["transient".to_string(), "transient".to_string()]
            );
            assert!(
                !error.to_string().contains("pr_auto_fix"),
                "no repair hand-off: {error}"
            );
        },
    );
}

#[test]
fn stopped_job_reruns_when_its_log_cannot_be_read() {
    isolated(
        "ci_rerun::cancelled::stopped_job_reruns_when_its_log_cannot_be_read",
        |sandbox| {
            for (conclusion, refusal) in [
                ("cancelled", "log_404_jobs"),
                ("timed_out", "log_404_jobs"),
                ("cancelled", "log_truncated_jobs"),
            ] {
                in_fresh_row(sandbox, |row| {
                    let (fx, host, _) = fixture(row);
                    patch_config(&fx, |config| {
                        config["conclusions"] = json!({"test": conclusion, "lint": "success"});
                        config["run_override"] = json!({"conclusion": conclusion});
                        config[refusal] = json!([1000]);
                    });
                    let error = complete_auto_fix(&fx, &host)
                        .expect_err("a job that stays stopped does not merge");
                    assert_eq!(
                        fx.forge_lines("retry-sends"),
                        vec!["run rerun 700 --failed".to_string()],
                        "{conclusion} via {refusal}"
                    );
                    assert_no_auto_fix(&host);
                    assert_eq!(rerun_classifications(&host), vec!["transient".to_string()]);
                    let message = error.to_string();
                    assert!(
                        message.contains("required CI jobs keep cancelling or timing out:"),
                        "{conclusion} via {refusal}: {message}"
                    );
                    assert!(message.contains("CI / test"), "{message}");
                });
            }
        },
    );
}

#[test]
fn mixed_failure_and_cancelled_job_is_not_transient() {
    isolated(
        "ci_rerun::cancelled::mixed_failure_and_cancelled_job_is_not_transient",
        |sandbox| {
            let (fx, host, _) = fixture(sandbox);
            patch_config(&fx, |config| {
                // The cancelled job is itself a failed required check, and its
                // log is unreadable. That must not hide the source failure.
                config["conclusions"] = json!({"independent": "cancelled"});
                config["log_404_jobs"] = json!([1002]);
            });
            let error = complete_auto_fix(&fx, &host).expect_err("source failure repairs");
            assert!(
                orbit_types::workflow::is_pr_auto_fix_check_repair(&error.to_string()),
                "{error}"
            );
            assert!(fx.forge_lines("retry-sends").is_empty());
            assert_eq!(pr_auto_fix_attempts(&host.history(TASK_ID), "42").len(), 1);
        },
    );
}

#[test]
fn exhausted_cancel_budget_names_the_jobs_and_skips_auto_fix() {
    isolated(
        "ci_rerun::cancelled::exhausted_cancel_budget_names_the_jobs_and_skips_auto_fix",
        |sandbox| {
            let (fx, host, _) = fixture(sandbox);
            patch_config(&fx, |config| {
                config["conclusions"] = json!({"test": "cancelled", "lint": "success"});
                config["run_override"] = json!({"conclusion": "cancelled"});
                config["log_404_jobs"] = json!([1000]);
            });
            super::super::auto_fix::seed_auto_fix_history(
                &host,
                "ci_check_rerun",
                &json!({"head": fx.candidate, "run_id": 700, "pr_number": 42}),
            );
            let error = complete_auto_fix(&fx, &host).expect_err("spent budget stops");
            let message = error.to_string();
            assert!(
                message.contains("required CI jobs keep cancelling or timing out:"),
                "{message}"
            );
            assert!(message.contains("CI / test"), "{message}");
            assert!(fx.forge_lines("retry-sends").is_empty(), "{message}");
            assert_no_auto_fix(&host);
            assert!(
                host.history(TASK_ID)
                    .iter()
                    .any(|entry| entry.event == "ci_check_rerun_exhausted"),
                "{:?}",
                host.history(TASK_ID)
            );
        },
    );
}

#[test]
fn ci_await_timeout_only_set_uses_one_rerun_then_names_the_job() {
    isolated(
        "ci_rerun::cancelled::ci_await_timeout_only_set_uses_one_rerun_then_names_the_job",
        |sandbox| {
            let (fx, host, _) = fixture(sandbox);
            // The configured infrastructure budget is 3. This set must not use it.
            locked(&host.pr_config).ci_infrastructure_retry.max_attempts = 3;
            patch_config(&fx, |config| {
                config["conclusions"] =
                    json!({"test": "success", "lint": "failure", "independent": "success"});
                config["run_override"] = json!({"conclusion": "failure"});
            });
            fs::write(fx.forge.join("retry-log-1001"), AWAIT_TIMEOUT).unwrap();
            // The rerun is a new attempt; the waiter keeps the same timeout log.
            fs::write(fx.forge.join("retry-log-1011"), AWAIT_TIMEOUT).unwrap();
            let error = complete_auto_fix(&fx, &host).expect_err("the spent single rerun stops");
            assert_eq!(
                fx.forge_lines("retry-sends"),
                vec!["run rerun 700 --failed".to_string()]
            );
            assert_no_auto_fix(&host);
            let message = error.to_string();
            assert!(
                message.contains("required CI jobs keep cancelling or timing out:"),
                "a ci-await timeout with no stopped job keeps the one shared rerun: {message}"
            );
            assert!(
                message.contains("CI / lint"),
                "the stop names the waiter: {message}"
            );
            assert!(
                !message.contains("ci_infrastructure_retry_exhausted"),
                "this set does not spend the infrastructure budget: {message}"
            );
            let history = host.history(TASK_ID);
            assert!(
                history
                    .iter()
                    .any(|entry| entry.event == "ci_check_rerun_exhausted"),
                "exhaustion is the keep-cancelling stop: {history:?}"
            );
            assert!(
                history
                    .iter()
                    .all(|entry| entry.event != "ci_infrastructure_retry_exhausted"),
                "no infrastructure hold: {history:?}"
            );
        },
    );
}

#[test]
fn claimed_landing_reruns_a_cancelled_job_whose_log_404s() {
    isolated(
        "ci_rerun::cancelled::claimed_landing_reruns_a_cancelled_job_whose_log_404s",
        |sandbox| {
            let (fx, host, _) = fixture(sandbox);
            patch_config(&fx, |config| {
                config["conclusions"] = json!({"test": "cancelled", "lint": "success"});
                config["run_override"] = json!({"conclusion": "cancelled"});
                config["log_404_jobs"] = json!([1000]);
            });
            let claimed = claimed(&fx, &host);
            let error = land_claimed_retry(&claimed).expect_err("the stop is recorded");
            assert_eq!(
                fx.forge_lines("retry-sends"),
                vec!["run rerun 700 --failed".to_string()]
            );
            let message = error.to_string();
            assert!(
                message.contains("required CI jobs keep cancelling or timing out:"),
                "{message}"
            );
            assert!(message.contains("CI / test"), "{message}");
            assert_no_auto_fix(&host);
        },
    );
}
