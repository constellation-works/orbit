//! A waiter whose only diagnostic is an authenticated dependency record
//! takes its dependencies' class, not `candidate`.
use super::*;

const SIGNATURE: &str = "ci-download: outcome=failed class=transient";
const DOWNLOAD_504: &str = "\
test\tTests\tcurl: (22) The requested URL returned error: 504
test\tTests\tci-download: attempt=9 curl_exit=22 http='504' seconds=0.400
test\tTests\tci-download: failure=total budget cannot accommodate backoff
test\tTests\tci-download: outcome=failed class=transient attempts=9 total_seconds=57.100 budget_seconds=60.0
test\tTests\t##[error]Process completed with exit code 1.
";

fn patch_config(fx: &Fixture, edit: impl FnOnce(&mut Value)) {
    let path = fx.forge.join("retry-config.json");
    let mut config: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    edit(&mut config);
    fs::write(path, serde_json::to_vec(&config).unwrap()).unwrap();
}

/// The waiter's record as the producer emits it on the rerun attempt:
/// same record, bound to the attempt's new job identities.
fn rerun_waiter_log(fx: &Fixture, tamper_dependency: bool, attempt: u64) -> String {
    let original = fs::read_to_string(fx.forge.join("retry-log-1001")).unwrap();
    original
        .lines()
        .map(|line| {
            let Some((_, body)) = line.split_once("orbit-dependency-evidence:") else {
                return format!("{line}\n");
            };
            let mut record: Value = serde_json::from_str(body).unwrap();
            let job_offset = 10 * (attempt - 1);
            record["run_attempt"] = json!(attempt);
            record["source"]["job_id"] = json!(1001 + job_offset);
            record["dependencies"][0]["job_id"] = json!(if tamper_dependency {
                1005
            } else {
                1000 + job_offset
            });
            format!("lint\tWait\torbit-dependency-evidence:{record}\n")
        })
        .collect()
}

fn land(fx: &Fixture, host: &DeliveryHost) -> String {
    let claimed = claimed(fx, host);
    let result = with_completion_clock(|| {
        execute_deterministic_action(
            &claimed,
            "handoff_land",
            &json!({}),
            &json!({"handoff_id":"handoff-retry","task_id":TASK_ID,"run_id":RUN_ID,"max_wait_seconds":300,"poll_interval_seconds":5}),
            false,
            &HashMap::new(),
            None,
        )
    });
    match result {
        Ok(value) => format!("{value}"),
        Err(error) => error.to_string(),
    }
}

fn setup(fx: &Fixture, shard_log: &str, tamper_dependency: bool) {
    patch_config(fx, |config| {
        config["repeat"] = json!(true);
        config["conclusions"] = json!({"independent": "success"});
    });
    fs::write(fx.forge.join("retry-log-1000"), shard_log).unwrap();
    if tamper_dependency {
        // Tamper the first attempt's record too, so no attempt authenticates.
        let original = fs::read_to_string(fx.forge.join("retry-log-1001")).unwrap();
        let tampered = original
            .lines()
            .map(|line| {
                let Some((_, body)) = line.split_once("orbit-dependency-evidence:") else {
                    return format!("{line}\n");
                };
                let mut record: Value = serde_json::from_str(body).unwrap();
                record["dependencies"][0]["job_id"] = json!(1005);
                format!("lint\tWait\torbit-dependency-evidence:{record}\n")
            })
            .collect::<String>();
        fs::write(fx.forge.join("retry-log-1001"), tampered).unwrap();
    }
    for attempt in 2..=3 {
        let job_id = 1000 + 10 * (attempt - 1);
        fs::write(fx.forge.join(format!("retry-log-{job_id}")), shard_log).unwrap();
        let rerun_log = rerun_waiter_log(fx, tamper_dependency, attempt);
        fs::write(
            fx.forge.join(format!("retry-log-{}", job_id + 1)),
            rerun_log,
        )
        .unwrap();
    }
}

#[test]
fn waiter_folded_into_transient_shard_classifies_transient() {
    isolated(
        "ci_rerun::dependency_class::waiter_folded_into_transient_shard_classifies_transient",
        |sandbox| {
            let (fx, host, _) = fixture(sandbox);
            locked(&host.pr_config).ci_infrastructure_signatures = vec![SIGNATURE.to_string()];
            locked(&host.pr_config).ci_infrastructure_retry.max_attempts = 2;
            setup(&fx, DOWNLOAD_504, false);
            let message = land(&fx, &host);
            assert!(
                message.contains("classified `transient`"),
                "the waiter's folded class is the shard's transient class: {message}"
            );
            assert!(
                message.contains("folded into 'CI / test'"),
                "the waiter is named as folded into the shard: {message}"
            );
            assert!(
                message.contains("'CI / lint'"),
                "the waiter is named: {message}"
            );
            assert_eq!(
                fx.forge_lines("retry-sends"),
                vec!["run rerun 700 --failed".to_string(); 2],
                "the configured reruns are spent on the transient shard: {message}"
            );
            assert!(
                message.contains("ci_infrastructure_retry_exhausted")
                    && message.contains("classification=transient attempts=2"),
                "the folded transient set records its bounded hold: {message}"
            );
        },
    );
}

#[test]
fn waiter_folded_into_candidate_shard_stays_candidate() {
    isolated(
        "ci_rerun::dependency_class::waiter_folded_into_candidate_shard_stays_candidate",
        |sandbox| {
            let (fx, host, _) = fixture(sandbox);
            setup(
                &fx,
                "test\tTests\terror: source failure at src/stable:1:1\n",
                false,
            );
            let message = land(&fx, &host);
            assert!(
                message.contains("classified `candidate`"),
                "a real candidate diagnostic on the shard still decides: {message}"
            );
            assert!(
                message.contains("folded into 'CI / test'"),
                "the waiter is named as folded into the shard: {message}"
            );
        },
    );
}

#[test]
fn unauthenticated_dependency_record_keeps_candidate_class() {
    isolated(
        "ci_rerun::dependency_class::unauthenticated_dependency_record_keeps_candidate_class",
        |sandbox| {
            let (fx, host, _) = fixture(sandbox);
            locked(&host.pr_config).ci_infrastructure_signatures = vec![SIGNATURE.to_string()];
            setup(&fx, DOWNLOAD_504, true);
            let message = land(&fx, &host);
            assert!(
                message.contains("classified `candidate`"),
                "an unauthenticated record keeps today's candidate class: {message}"
            );
            assert!(
                !message.contains("folded into"),
                "nothing folds on an unauthenticated record: {message}"
            );
        },
    );
}
