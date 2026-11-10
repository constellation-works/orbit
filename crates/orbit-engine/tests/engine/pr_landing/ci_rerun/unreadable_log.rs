//! One unreadable failed-job log must not void the readable jobs' class.
use super::*;
use orbit_types::workflow::DeliveryRefusal;

const FAILING_COMMAND: &str = "test\tTests\terror: source failure at src/stable:1:1\n";

fn patch_config(fx: &Fixture, edit: impl FnOnce(&mut Value)) {
    let path = fx.forge.join("retry-config.json");
    let mut config: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    edit(&mut config);
    fs::write(path, serde_json::to_vec(&config).unwrap()).unwrap();
}

/// More than the job-log display budget, in short lines, with no failing command.
/// The reader reports the log truncated and selects no diagnostic.
fn truncated_log() -> String {
    "x\n".repeat(35 * 1024)
}

fn assert_truncated_without_diagnostic(fx: &Fixture, job_id: u64) {
    let read = run_private_vcs_operation(
        "ci.job_log",
        &json!({
            "workspace_path": fx.repo,
            "run_id": "700",
            "job_id": job_id.to_string(),
        }),
    )
    .unwrap_or_else(|error| panic!("job {job_id} log read failed: {error}"));
    assert_eq!(read["truncated"], true, "{read}");
    assert_eq!(read["source_complete"], true, "{read}");
    assert!(
        !read["diagnostic"].is_string(),
        "a truncated log with no failing command selects no diagnostic: {read}"
    );
    assert!(
        read["failure_regions"].is_null(),
        "a truncated log with no failing command has no failure regions: {read}"
    );
}

#[test]
fn readable_base_failure_classifies_when_one_log_is_truncated() {
    isolated(
        "ci_rerun::unreadable_log::readable_base_failure_classifies_when_one_log_is_truncated",
        |sandbox| {
            let (fx, host, _) = fixture(sandbox);
            patch_config(&fx, |config| {
                config["base_failures"] = json!(["test"]);
            });
            fs::write(fx.forge.join("retry-log-2000"), FAILING_COMMAND).unwrap();
            fs::write(fx.forge.join("retry-log-1001"), truncated_log()).unwrap();
            assert_truncated_without_diagnostic(&fx, 1001);
            let claimed = claimed(&fx, &host);
            let error = land_claimed_retry(&claimed)
                .expect_err("the readable job is the base's failure, so landing waits");
            let message = error.to_string();
            assert!(
                message.contains("classified `inherited`"),
                "the readable job decides: {message}"
            );
            assert!(
                message.contains("error: source failure at src/stable:<n>:<n>"),
                "the readable signature is the evidence: {message}"
            );
            assert!(
                message.contains("unreadable job logs left unknown: 'CI / lint'"),
                "the unreadable job is named: {message}"
            );
            assert!(
                message.contains("the job log is truncated and has no complete failing command"),
                "the truncated-log refusal is unchanged: {message}"
            );
            assert!(
                !message.contains("was not classified"),
                "one unreadable log must not void the classification: {message}"
            );
            assert!(
                fx.forge_lines("retry-sends").is_empty(),
                "an inherited failure spends no rerun: {message}"
            );
        },
    );
}

#[test]
fn every_unreadable_failed_job_log_stays_unclassified() {
    isolated(
        "ci_rerun::unreadable_log::every_unreadable_failed_job_log_stays_unclassified",
        |sandbox| {
            let (fx, host, _) = fixture(sandbox);
            let blob = truncated_log();
            fs::write(fx.forge.join("retry-log-1000"), &blob).unwrap();
            fs::write(fx.forge.join("retry-log-1001"), &blob).unwrap();
            assert_truncated_without_diagnostic(&fx, 1000);
            assert_truncated_without_diagnostic(&fx, 1001);
            let claimed = claimed(&fx, &host);
            let error = land_claimed_retry(&claimed)
                .expect_err("no readable job decides, so the candidate stays in review");
            let message = error.to_string();
            assert!(
                matches!(
                    error,
                    OrbitError::DeliveryRefused {
                        refusal: DeliveryRefusal::CheckEvidenceInconclusive,
                        ..
                    }
                ),
                "every unreadable log holds the landing: {message}"
            );
            assert!(
                message.contains("was not classified"),
                "every unreadable log stays unclassified: {message}"
            );
            assert!(
                message.contains("the log of 'CI / test' could not be read:"),
                "{message}"
            );
            assert!(
                message.contains("the log of 'CI / lint' could not be read:"),
                "{message}"
            );
            assert!(
                !message.contains("classified `"),
                "an unknown log is not a deciding class: {message}"
            );
            assert!(
                fx.forge_lines("retry-sends").is_empty(),
                "an unclassified failure spends no rerun: {message}"
            );
        },
    );
}
