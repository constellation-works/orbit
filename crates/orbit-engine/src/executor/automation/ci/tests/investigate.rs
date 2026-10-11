//! Admission: combinatorial attribution/safety checks for the private CI
//! boundary, which public engine fixtures cannot supply scripted logs to.

use serde_json::json;

use super::super::{investigate::bound_diagnostic, query::bounded_run_log};

#[test]
fn nextest_regions_bind_only_complete_evidence_to_the_known_failed_step() {
    let prefix = "CI\tTests\t";
    let padding = format!("{prefix}PASS [ 0.001s] suite passing_test\n").repeat(32_768);
    let raw = format!(
        "{prefix}##[group]Run cargo nextest run\n{padding}{prefix}FAIL [ 0.123s] suite failing_test\n{prefix}──────── STDERR ────────\n{prefix}thread 'failing_test' panicked at tests/sample.rs:12:3:\n{prefix}expected a completed operation\n{padding}{prefix}##[error]Process completed with exit code 100.\n"
    );
    let log = bounded_run_log(&raw, 128);
    let failure = json!({"failed_jobs": [{"name": "CI", "failed_steps": [{"name": "Tests"}]}]});
    let unit =
        bound_diagnostic(&log, &failure, 42).expect("mid-log regions bind to the failed step");
    assert_eq!(unit["job_id"], 42);
    assert_eq!(unit["step"], "Tests");
    assert!(
        unit["text"]
            .as_str()
            .unwrap()
            .contains("expected a completed operation")
    );
    assert!(unit["byte_ranges"].as_array().is_some());
    for flag in ["command_complete", "selection_complete"] {
        let mut incomplete = log.clone();
        incomplete.failure_regions.as_mut().unwrap()[flag] = json!(false);
        assert!(
            bound_diagnostic(&incomplete, &failure, 42).is_none(),
            "incomplete {flag} is not actionable CI evidence"
        );
    }
    let other = json!({"failed_jobs": [{"name": "Other", "failed_steps": [{"name": "Tests"}]}]});
    assert!(
        bound_diagnostic(&log, &other, 42).is_none(),
        "regions must not borrow another job's identity"
    );
    let conflicting = raw.replacen("CI\tTests\tPASS", "Other\tTests\tPASS", 1);
    let conflicting = bounded_run_log(&conflicting, 128);
    assert!(
        bound_diagnostic(&conflicting, &failure, 42).is_none(),
        "omitted lines must not hide conflicting command attribution"
    );
    let mut raw_job = bounded_run_log(&raw.replace(prefix, ""), 128);
    raw_job.source = orbit_tools::github_cli::SOURCE_JOB_API_LOG.to_string();
    assert!(bound_diagnostic(&raw_job, &failure, 42).is_some());
    let ambiguous = format!(
        "##[group]Run earlier\n##[error]Process completed with exit code 1.\n{}",
        raw.replace(prefix, "")
    );
    let mut ambiguous = bounded_run_log(&ambiguous, 128);
    ambiguous.source = orbit_tools::github_cli::SOURCE_JOB_API_LOG.to_string();
    assert!(
        ambiguous.failure_regions.is_some(),
        "raw multi-command failures remain available to a human"
    );
    assert!(
        bound_diagnostic(&ambiguous, &failure, 42).is_none(),
        "raw logs cannot attribute multiple failed commands to one known step"
    );
}
