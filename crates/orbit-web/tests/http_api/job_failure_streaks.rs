//! The failing-job warning's reads: `/api/jobs` reports each job's failure
//! streak and `/api/jobs/failure-streaks` returns the jobs at or above
//! `jobs.failure_streak_warn`, in isolated children.

use chrono::{Duration, Utc};
use orbit_core::JobRunState;
use serde_json::{Value, json};

use super::support::{Fixture, isolated, json_ok};

/// Seed `states` as `job`'s finished runs, oldest first, ending now.
fn seed_runs(fixture: &Fixture, job: &str, states: &[JobRunState]) {
    let now = Utc::now();
    for (index, state) in states.iter().enumerate() {
        let mut run = fixture.seed_run(&format!("{job}-{index}"), job, *state);
        let at = now - Duration::minutes((states.len() - index) as i64);
        run.created_at = at;
        run.started_at = Some(at);
        run.finished_at = Some(at);
        fixture.save_run(&run);
    }
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

#[test]
fn a_zero_threshold_turns_the_warning_off() {
    isolated(
        "job_failure_streaks::a_zero_threshold_turns_the_warning_off",
        || {
            let fixture = Fixture::new();
            seed_fixture(&fixture);
            let config = fixture.global.join("config.toml");
            let existing = std::fs::read_to_string(&config).unwrap_or_default();
            std::fs::write(
                &config,
                format!("{existing}\n[jobs]\nfailure_streak_warn = 0\n"),
            )
            .unwrap();
            let server = fixture.server(false);

            let page = json_ok(server.get("/api/jobs/failure-streaks?workspace=ws_http_fixture"));
            assert_eq!(page["threshold"], 0);
            assert!(page["failing"].as_array().unwrap().is_empty(), "{page}");
        },
    );
}
