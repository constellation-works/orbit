//! [ORB-23813] A failed set whose every job stopped in a declared
//! dependency-handoff step reruns from the provider's step names, even when
//! the job log is truncated; another failed step keeps today's route.
use super::*;

const HANDOFF_STEP: &str = "Download nextest archive";

fn handoff_failure(row: &Path, independent: &str) -> (Fixture, DeliveryHost) {
    let (fx, host, _) = fixture(row);
    let path = fx.forge.join("retry-config.json");
    let mut config: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    config["conclusions"] = json!({"lint": "success", "independent": independent});
    config["repeat"] = json!(true);
    // The test job (and its rerun) fails in the handoff step, and its log is
    // truncated with no complete failing command, as in PR #8940.
    let step = json!([{"name": HANDOFF_STEP, "number": 1, "status": "completed", "conclusion": "failure"}]);
    config["job_overrides"] = json!({"1000": {"steps": step}, "1010": {"steps": step}});
    config["log_truncated_jobs"] = json!([1000, 1010]);
    fs::write(path, serde_json::to_vec(&config).unwrap()).unwrap();
    fs::write(
        fx.forge.join("retry-log-1002"),
        "independent\tTests\terror: source failure at src/stable:1:1\n",
    )
    .unwrap();
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
fn handoff_step_failure_with_truncated_log_reruns_as_transient() {
    isolated(
        "ci_rerun::handoff_step::handoff_step_failure_with_truncated_log_reruns_as_transient",
        |sandbox| {
            in_fresh_row(sandbox, |row| {
                let (fx, host) = handoff_failure(row, "success");
                {
                    let mut config = locked(&host.pr_config);
                    config.ci_handoff_steps = vec!["Download nextest*".into()];
                    config.ci_infrastructure_retry.max_attempts = 1;
                }
                let error = complete_auto_fix(&fx, &host)
                    .expect_err("a handoff that keeps failing does not merge");
                let message = error.to_string();
                assert_eq!(
                    fx.forge_lines("retry-sends"),
                    vec!["run rerun 700 --failed".to_string()],
                    "{message}"
                );
                assert!(
                    pr_auto_fix_attempts(&host.history(TASK_ID), "42").is_empty(),
                    "no repair attempt: {:?}",
                    host.history(TASK_ID)
                );
                assert!(
                    !orbit_types::workflow::is_pr_auto_fix_check_repair(&message),
                    "no repair hand-off: {message}"
                );
                assert!(
                    message.contains("classification=transient attempts=1"),
                    "the exhausted infrastructure budget holds as transient: {message}"
                );
            });
            // One failed job outside a handoff step keeps today's route: the
            // set is not transient and nothing is rerun.
            in_fresh_row(sandbox, |row| {
                let (fx, host) = handoff_failure(row, "failure");
                locked(&host.pr_config).ci_handoff_steps = vec![HANDOFF_STEP.into()];
                let error = complete_auto_fix(&fx, &host).expect_err("a mixed set stops");
                assert!(
                    fx.forge_lines("retry-sends").is_empty(),
                    "a mixed set is not transient: {error}"
                );
                assert!(
                    error
                        .to_string()
                        .contains("the job log is truncated and has no complete failing command"),
                    "{error}"
                );
            });
        },
    );
}
