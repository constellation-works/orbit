//! [ORB-23164] Completed job logs are read once, and a rate-limited read
//! defers the landing to the provider's reset instead of leaving the
//! failure unclassified.
use super::*;
use orbit_types::workflow::DeliveryRefusal;

fn patch_config(fx: &Fixture, edit: impl FnOnce(&mut Value)) {
    let path = fx.forge.join("retry-config.json");
    let mut config: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    edit(&mut config);
    fs::write(path, serde_json::to_vec(&config).unwrap()).unwrap();
}

fn log_reads(fx: &Fixture, job: &str) -> usize {
    fx.forge_lines("retry-log-reads")
        .iter()
        .filter(|read| read.as_str() == job)
        .count()
}

#[test]
fn two_landing_passes_read_a_completed_failed_jobs_log_once() {
    isolated(
        "ci_rerun::rate_limit::two_landing_passes_read_a_completed_failed_jobs_log_once",
        |sandbox| {
            let (fx, host, _) = fixture(sandbox);
            // The base's own `test` job fails too; each pass reads the same
            // completed failed jobs and stops with the same classification.
            patch_config(&fx, |config| {
                config["base_failures"] = json!(["test"]);
            });
            fs::write(
                fx.forge.join("retry-log-2000"),
                "test\tTests\terror: source failure at src/stable:1:1\n",
            )
            .unwrap();
            let mut claimed = claimed(&fx, &host);
            claimed.data_root = sandbox.join("host-data");
            let outcomes: Vec<String> = (0..2)
                .map(|_| {
                    land_claimed_retry(&claimed)
                        .expect_err("the classified failure stops the pass")
                        .to_string()
                })
                .collect();
            assert!(outcomes[0].contains("classified `"), "{}", outcomes[0]);
            assert_eq!(
                outcomes[0], outcomes[1],
                "a reused log classifies the same way"
            );
            assert_eq!(
                log_reads(&fx, "1000"),
                1,
                "the completed job's log is fetched once across both passes: {:?}",
                fx.forge_lines("retry-log-reads")
            );
            assert!(
                fx.forge_lines("retry-sends").is_empty(),
                "an inherited failure spends no rerun"
            );
        },
    );
}

#[test]
fn a_rate_limited_log_read_defers_the_landing_to_the_reset() {
    isolated(
        "ci_rerun::rate_limit::a_rate_limited_log_read_defers_the_landing_to_the_reset",
        |sandbox| {
            let (fx, host, _) = fixture(sandbox);
            // 2033-05-18T03:33:20Z, far past the landing's deadline.
            patch_config(&fx, |config| {
                config["log_rate_limited_jobs"] = json!([1000, 1001]);
                config["rate_limit_reset"] = json!(2_000_000_000_u64);
            });
            let held = claimed(&fx, &host);
            let error = land_claimed_retry(&held)
                .expect_err("a rate-limited landing is held for the forge");
            let message = error.to_string();
            assert!(
                message.contains("the provider rate-limited the read"),
                "{message}"
            );
            assert!(
                message.contains("resets at 2033-05-18T03:33:20Z"),
                "the deferral carries the reset: {message}"
            );
            assert!(
                !message.contains("was not classified"),
                "a rate limit is a deferral, not an unclassified failure: {message}"
            );
            assert!(
                matches!(
                    error,
                    OrbitError::DeliveryRefused {
                        refusal: DeliveryRefusal::ForgeObservationUnavailable,
                        ..
                    }
                ),
                "the landing is a resumable forge hold: {error:?}"
            );
            assert!(fx.forge_lines("retry-sends").is_empty());

            // A reset inside the deadline is waited for and recorded, then
            // read again; a read still throttled after the bounded waits holds.
            fs::remove_file(fx.forge.join("retry-log-reads")).ok();
            patch_config(&fx, |config| {
                config["rate_limit_reset"] = json!(1_002_u64);
            });
            let waited = claimed(&fx, &host);
            let message = land_claimed_retry(&waited)
                .expect_err("the read stays throttled")
                .to_string();
            let waits = locked(&waited.waits).clone();
            assert_eq!(waits.len(), 3, "{waits:?}");
            for wait in &waits {
                assert_eq!(wait["state"], "read_deferred", "{wait}");
                assert!(
                    wait["not_before_ms"].as_u64().unwrap() >= 1_002_000,
                    "{wait}"
                );
                assert_eq!(wait["reset_epoch_seconds"], 1_002, "{wait}");
            }
            assert!(message.contains("after 3 bounded waits"), "{message}");
            assert!(!message.contains("was not classified"), "{message}");
            assert_eq!(
                log_reads(&fx, "1000"),
                4,
                "one read per pass, none between the waits"
            );
        },
    );
}
