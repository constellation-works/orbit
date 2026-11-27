//! The failing-job warning's reads: `/api/jobs` reports each job's failure
//! streak and `/api/jobs/failure-streaks` returns the jobs at or above
//! the fixed warning threshold, in isolated children.

use chrono::{Duration, Utc};
use orbit_types::workflow::JobRunState;
use serde_json::{Value, json};

use super::support::{Fixture, isolated, json_ok};

/// Seed `states` as `job`'s finished runs, oldest first, ending now.
fn seed_runs(
    fixture: &Fixture,
    job: &str,
    states: &[JobRunState],
) -> Vec<orbit_types::workflow::JobRun> {
    let now = Utc::now();
    let mut runs = Vec::new();
    for (index, state) in states.iter().enumerate() {
        let mut run = fixture.seed_run(&format!("{job}-{index}"), job, *state);
        let at = now - Duration::minutes((states.len() - index) as i64);
        run.created_at = at;
        run.started_at = Some(at);
        run.finished_at = Some(at);
        fixture.save_run(&run);
        runs.push(run);
    }
    runs
}

fn seed_fixture(fixture: &Fixture) {
    use JobRunState::{Cancelled, Failed, Success};
    fixture.job("broken");
    fixture.job("flaky");
    fixture.job("healthy");
    // Six failures in a row, one of them an operator cancel that is skipped.
    seed_runs(
        fixture,
        "broken",
        &[
            Success, Failed, Failed, Cancelled, Failed, Failed, Failed, Failed,
        ],
    );
    // Two failures after a success: a streak, but below the default threshold.
    seed_runs(fixture, "flaky", &[Failed, Success, Failed, Failed]);
    seed_runs(fixture, "healthy", &[Failed, Failed, Success]);
}

#[test]
fn failure_streaks_list_the_jobs_at_the_default_threshold() {
    isolated(
        "job_failure_streaks::failure_streaks_list_the_jobs_at_the_default_threshold",
        || {
            let fixture = Fixture::new();
            seed_fixture(&fixture);
            let server = fixture.server(false);

            let page = json_ok(server.get("/api/jobs/failure-streaks?workspace=ws_http_fixture"));
            assert_eq!(page["threshold"], 5);
            let failing = page["failing"].as_array().unwrap();
            assert_eq!(failing.len(), 1, "{page}");
            assert_eq!(failing[0]["job_id"], "broken");
            assert_eq!(failing[0]["count"], 6);
            assert_eq!(failing[0]["latest_run_id"], "broken-7");
            assert_eq!(failing[0]["truncated"], false);

            let jobs = json_ok(server.get("/api/jobs?workspace=ws_http_fixture"));
            let streak = |name: &str| -> Value {
                jobs.as_array()
                    .unwrap()
                    .iter()
                    .find(|job| job["job_id"] == name)
                    .unwrap_or_else(|| panic!("{name} missing from {jobs}"))["failure_streak"]
                    .clone()
            };
            assert_eq!(streak("broken")["count"], 6);
            assert_eq!(
                streak("flaky")["count"],
                2,
                "below the threshold, still reported"
            );
            assert!(streak("healthy").is_null());
        },
    );
}

#[test]
fn non_runnable_jobs_are_hidden_from_the_failing_badge_and_report_runnability() {
    isolated(
        "job_failure_streaks::non_runnable_jobs_are_hidden_from_the_failing_badge_and_report_runnability",
        || {
            let fixture = Fixture::new();
            seed_fixture(&fixture);
            // A delivery pipeline: server reports it not manual-runnable, so
            // its streak must not count toward the catalogue-only badge.
            fixture.job("task_pr_pipeline");
            seed_runs(
                &fixture,
                "task_pr_pipeline",
                &[
                    JobRunState::Failed,
                    JobRunState::Failed,
                    JobRunState::Failed,
                    JobRunState::Failed,
                    JobRunState::Failed,
                    JobRunState::Failed,
                ],
            );
            let server = fixture.server(true);

            let page = json_ok(server.get("/api/jobs/failure-streaks?workspace=ws_http_fixture"));
            let failing = page["failing"].as_array().unwrap();
            assert_eq!(failing.len(), 1, "{page}");
            assert_eq!(failing[0]["job_id"], "broken");

            // The manual run route refuses the same job the catalogue hides.
            let refused = server.send(
                "POST",
                "/api/jobs/task_pr_pipeline/run?workspace=ws_http_fixture",
                json!({}),
            );
            assert!(!refused.status().is_success(), "{}", refused.status());

            let jobs = json_ok(server.get("/api/jobs?workspace=ws_http_fixture"));
            let field = |name: &str, key: &str| -> Value {
                jobs.as_array()
                    .unwrap()
                    .iter()
                    .find(|job| job["job_id"] == name)
                    .unwrap_or_else(|| panic!("{name} missing from {jobs}"))[key]
                    .clone()
            };
            assert_eq!(field("broken", "manual_runnable"), true);
            assert_eq!(field("broken", "supports_no_input_submission"), true);
            assert_eq!(field("task_pr_pipeline", "manual_runnable"), false);
            assert_eq!(
                field("task_pr_pipeline", "supports_no_input_submission"),
                false
            );
        },
    );
}

/// Both job surfaces claim agreement only for what was read: a 26-run streak
/// whose oldest, uninspected run failed differently agrees only in its newest
/// 25, and a lone failure with no recorded error is unknown.
#[test]
fn failure_streaks_claim_agreement_only_for_inspected_runs() {
    isolated(
        "job_failure_streaks::failure_streaks_claim_agreement_only_for_inspected_runs",
        || {
            use super::support::{run_step, save_record_step};
            use orbit_types::workflow::JobTargetType;
            let fixture = Fixture::new();
            fixture.job("long");
            fixture.job("silent");
            let mut states = vec![JobRunState::Success];
            states.extend([JobRunState::Failed; 26]);
            let runs = seed_runs(&fixture, "long", &states);
            for (index, run) in runs.iter().enumerate().skip(1) {
                let mut step = run_step(JobTargetType::Activity, "review", JobRunState::Failed);
                step.error_message = Some(if index == 1 {
                    "disk full on the runner".to_string()
                } else {
                    format!("[provider_capacity] retry {index}")
                });
                save_record_step(&fixture, run, &step);
            }
            seed_runs(
                &fixture,
                "silent",
                &[JobRunState::Success, JobRunState::Failed],
            );
            let server = fixture.server(false);

            let page = json_ok(server.get("/api/jobs/failure-streaks?workspace=ws_http_fixture"));
            let long = page["failing"]
                .as_array()
                .unwrap()
                .iter()
                .find(|streak| streak["job_id"] == "long")
                .cloned()
                .unwrap_or_else(|| panic!("long missing from {page}"));
            assert_eq!(long["count"], 26, "{long}");
            assert_eq!(long["same_error"], false, "{long}");
            assert_eq!(long["error_agreement"], "partial", "{long}");
            assert_eq!(long["errors_inspected"], 25, "{long}");

            let jobs = json_ok(server.get("/api/jobs?workspace=ws_http_fixture"));
            let streak = |name: &str| -> Value {
                jobs.as_array()
                    .unwrap()
                    .iter()
                    .find(|job| job["job_id"] == name)
                    .unwrap_or_else(|| panic!("{name} missing from {jobs}"))["failure_streak"]
                    .clone()
            };
            assert_eq!(streak("long")["error_agreement"], "partial");
            assert_eq!(streak("long")["same_error"], false);
            let silent = streak("silent");
            assert_eq!(silent["count"], 1, "{silent}");
            assert_eq!(silent["latest_error"], Value::Null, "{silent}");
            assert_eq!(silent["same_error"], false, "{silent}");
            assert_eq!(silent["error_agreement"], "unknown", "{silent}");
        },
    );
}

#[test]
fn failed_steps_without_errors_do_not_establish_agreement() {
    isolated(
        "job_failure_streaks::failed_steps_without_errors_do_not_establish_agreement",
        || {
            use super::support::{run_step, save_record_step};
            use orbit_types::workflow::JobTargetType;
            let fixture = Fixture::new();
            fixture.job("silent-steps");
            for run in seed_runs(&fixture, "silent-steps", &[JobRunState::Failed; 5]) {
                let mut step = run_step(JobTargetType::Activity, "review", JobRunState::Failed);
                step.started_at = run.started_at;
                step.finished_at = run.finished_at;
                save_record_step(&fixture, &run, &step);
            }
            let server = fixture.server(false);
            let page = json_ok(server.get("/api/jobs/failure-streaks?workspace=ws_http_fixture"));
            let failing = &page["failing"][0];
            let jobs = json_ok(server.get("/api/jobs?workspace=ws_http_fixture"));
            let streak = &jobs
                .as_array()
                .unwrap()
                .iter()
                .find(|job| job["job_id"] == "silent-steps")
                .unwrap()["failure_streak"];
            for result in [failing, streak] {
                assert_eq!(result["job_id"], "silent-steps");
                assert_eq!(result["count"], 5);
                assert_eq!(result["latest_error"], Value::Null);
                assert_eq!(result["error_agreement"], "unknown");
                assert_eq!(result["same_error"], false);
            }
        },
    );
}
