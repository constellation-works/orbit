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
        &orbit_common::Env::capture(),
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
fn owner_auto_fix_reruns_a_truncated_required_job_once() {
    isolated(
        "ci_rerun::unreadable_log::owner_auto_fix_reruns_a_truncated_required_job_once",
        |sandbox| {
            for case in [
                "passes",
                "unavailable_passes",
                "still_unreadable",
                "readable_failure",
            ] {
                in_fresh_row(sandbox, |row| {
                    let (fx, host, _) = fixture(row);
                    patch_config(&fx, |config| {
                        config["conclusions"] = json!({"lint": "success"});
                        config["repeat"] =
                            json!(matches!(case, "still_unreadable" | "readable_failure"));
                        if case == "unavailable_passes" {
                            config["log_404_jobs"] = json!([1000]);
                        }
                    });
                    if case != "unavailable_passes" {
                        fs::write(fx.forge.join("retry-log-1000"), truncated_log()).unwrap();
                        assert_truncated_without_diagnostic(&fx, 1000);
                    }
                    fs::write(
                        fx.forge.join("retry-log-1010"),
                        if case == "still_unreadable" {
                            truncated_log()
                        } else {
                            FAILING_COMMAND.to_owned()
                        },
                    )
                    .unwrap();
                    let mut input = fx.complete_ungated_input();
                    input["auto_fix"] = json!(true);
                    input["completion"] = json!("review");
                    let outcome = complete_with_logical_clock(&host, &input);
                    match case {
                        "passes" | "unavailable_passes" => {
                            let result = outcome.unwrap_or_else(|error| {
                                panic!("{case}: green checks finish auto-fix: {error}")
                            });
                            assert_eq!(result["merge"]["reason"], "auto_fix_mergeable", "{result}");
                        }
                        "still_unreadable" => {
                            let error =
                                outcome.expect_err("the second unreadable log stops auto-fix");
                            assert!(
                                matches!(
                                    &error,
                                    OrbitError::DeliveryRefused {
                                        refusal: DeliveryRefusal::CheckEvidenceInconclusive,
                                        ..
                                    }
                                ),
                                "{error}"
                            );
                            assert!(error.to_string().contains(
                                "auto_fix_stopped: pull request #42 cannot be repaired automatically (the log of 'CI / test' could not be read: execution failed: the job log is truncated and has no complete failing command)"
                            ), "{error}");
                        }
                        _ => {
                            let error =
                                outcome.expect_err("a readable failure takes the repair route");
                            assert!(
                                orbit_types::workflow::is_pr_auto_fix_check_repair(
                                    &error.to_string()
                                ),
                                "{error}"
                            );
                        }
                    }
                    assert_eq!(
                        fx.forge_lines("retry-sends"),
                        vec!["run rerun 700 --failed"],
                        "{case}"
                    );
                    assert_eq!(fx.remote_tip(BRANCH), fx.candidate, "{case}");
                    let history = host.history(TASK_ID);
                    let reservations = history
                        .iter()
                        .filter_map(CiRerunPlan::from_reservation_entry)
                        .collect::<Vec<_>>();
                    let [reservation] = reservations.as_slice() else {
                        panic!("{case}: expected one authenticated reservation");
                    };
                    assert_eq!(reservation.head_sha, fx.candidate, "{case}");
                    assert_eq!(reservation.run_id, 700, "{case}");
                    assert_eq!(reservation.run_attempt, 1, "{case}");
                    assert_eq!(reservation.roots[0].job_id, 1000, "{case}");
                    assert_eq!(
                        pr_auto_fix_attempts(&history, "42").len(),
                        usize::from(case == "readable_failure"),
                        "{case}"
                    );
                    assert!(fx.merge_requests().is_empty(), "{case}");
                });
            }
        },
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
                        refusal: DeliveryRefusal::ForgeObservationUnavailable,
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
