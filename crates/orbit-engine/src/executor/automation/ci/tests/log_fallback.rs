//! Collection when a run-scoped log read comes back empty.
//!
//! `orbit-tools` owns recovering the bytes from a job's own log; what these
//! tests pin is the half collection owns: fallback evidence has to reach the
//! snapshot as a real, investigated failure with its own provenance, and a run
//! whose fallback recovered nothing must not withhold another run's complete
//! finding.

use serde_json::{Value, json};

use super::super::collect::collect;
use super::support::{FakeQueries, failed_job, run};

const HEAD: &str = "1111111111111111111111111111111111111111";
const CHECKOUT: &str = "3d9fc7c65934cdc98cec3954a37e10ba6d387e55";

fn input() -> Value {
    json!({"integration_branch": "topic"})
}

/// A job log as the log API serves it: no job/step columns, the checkout the
/// runner recorded for itself, and the diagnostic at the end.
fn job_log() -> String {
    format!(
        "2026-09-06T21:15:34.9569214Z [command]/usr/bin/git log -1 --format=%H\n\
         2026-09-06T21:15:34.9602377Z {CHECKOUT}\n\
         2026-09-06T21:28:07.0459354Z error: public documentation for `connect` links to private item `reject_root_override`\n\
         2026-09-06T21:28:07.0460831Z   --> crates/orbit-web/src/connect.rs:92:7\n\
         2026-09-06T21:28:07.4229928Z ##[error]Process completed with exit code 101.\n"
    )
}

fn source_job() -> Value {
    json!({
        "job_id": 101560010340_u64,
        "name": "docs",
        "conclusion": "failure",
        "url": "https://github.com/acme/orbit/actions/runs/10/job/101560010340",
    })
}

fn failing_run(run_id: u64) -> Value {
    run(
        run_id,
        "ci",
        HEAD,
        "completed",
        Some("failure"),
        "2026-09-06T21:15:00Z",
    )
}

fn failure_by_id(evidence: &Value, run_id: u64) -> Option<&Value> {
    evidence["current_failures"]
        .as_array()
        .expect("current failures")
        .iter()
        .find(|failure| failure["run_id"] == json!(run_id))
}

#[test]
fn a_run_whose_logs_came_from_a_job_investigates_with_that_evidence() {
    let queries = FakeQueries::authenticated()
        .with_head("topic", HEAD)
        .with_head("main", HEAD)
        .with_runs(vec![vec![failing_run(10)]])
        .with_run_view(
            "10",
            json!({"failed_jobs": [failed_job(101560010340, "docs")]}),
        )
        .with_job_log_fallback("10", false, &job_log(), vec![source_job()]);

    let evidence = collect(&queries, &input()).expect("collect");

    let failure = failure_by_id(&evidence, 10).expect("run 10 is a current failure");
    assert_eq!(failure["investigated"], json!(true));
    assert_eq!(failure["log_source"], json!("job_api_log"));
    assert_eq!(failure["log_source_jobs"], json!([source_job()]));
    assert!(
        failure["log_excerpt"]
            .as_str()
            .is_some_and(|log| log.contains("reject_root_override")
                && log.contains("crates/orbit-web/src/connect.rs:92")),
        "the recovered diagnostic must reach the snapshot: {}",
        failure["log_excerpt"]
    );
    // The commit under test is read from the runner's own output, not from the
    // event-reported head SHA the run advertised.
    assert_eq!(failure["actual_checkout_shas"], json!([CHECKOUT]));
    assert_ne!(failure["actual_checkout_shas"], json!([HEAD]));
    assert_eq!(failure["checkout_identity"]["state"], json!("observed"));
    assert_eq!(
        failure["checkout_identity"]["provenance"]["read_via"],
        json!("job_api_log")
    );
    assert_eq!(
        failure["checkout_identity"]["provenance"]["jobs"],
        json!([source_job()])
    );
    assert_eq!(evidence["outcome_hint"], json!("current_failures"));
    assert_eq!(evidence["retryable_errors"], json!([]));
    // Evidence recovered from the failed job's own log already carries the
    // checkout, so no full-log read had to be spent on it.
    assert_eq!(evidence["truncation"]["checkout_log_reads"], json!(0));
}

#[test]
fn a_failed_fallback_names_its_cause_and_never_reads_as_a_clean_run() {
    let queries = FakeQueries::authenticated()
        .with_head("topic", HEAD)
        .with_head("main", HEAD)
        .with_runs(vec![vec![failing_run(10)]])
        .with_run_view(
            "10",
            json!({"failed_jobs": [failed_job(101560010340, "docs")]}),
        )
        .with_log_fallback_error(
            "10",
            false,
            "job 101560010340 (`docs`): Not Found (HTTP 404)",
        )
        .with_log_fallback_error(
            "10",
            true,
            "job 101560010340 (`docs`): Not Found (HTTP 404)",
        );

    let evidence = collect(&queries, &input()).expect("collect");

    assert_eq!(evidence["outcome_hint"], json!("retryable_error"));
    // The run stays listed — a failed completed run is still red — but it is
    // explicitly not investigated, so nothing downstream can file it as an
    // evidenced regression.
    let failure = failure_by_id(&evidence, 10).expect("the red run stays visible");
    assert_eq!(failure["investigated"], json!(false));
    assert_eq!(failure["log_excerpt"], json!(""));
    assert_eq!(failure["actual_checkout_shas"], json!([]));
    let errors = evidence["retryable_errors"]
        .as_array()
        .expect("retryable errors");
    let log_error = errors
        .iter()
        .find(|error| error["operation"] == json!("run_logs"))
        .expect("the empty log read is reported");
    let message = log_error["message"].as_str().expect("message");
    assert!(
        message.contains("query returned no failed-step log text")
            && message.contains("per-job log fallback recovered none")
            && message.contains("Not Found (HTTP 404)"),
        "the gap must name the fallback's own outcome: {message}"
    );
}

#[test]
fn one_runs_failed_fallback_does_not_withhold_anothers_complete_finding() {
    let queries = FakeQueries::authenticated()
        .with_head("topic", HEAD)
        .with_head("main", HEAD)
        .with_runs(vec![vec![
            failing_run(10),
            run(
                11,
                "docs",
                HEAD,
                "completed",
                Some("failure"),
                "2026-09-06T21:16:00Z",
            ),
        ]])
        .with_run_view(
            "10",
            json!({"failed_jobs": [failed_job(101560010340, "docs")]}),
        )
        .with_run_view("11", json!({"failed_jobs": [failed_job(202, "build")]}))
        .with_log_fallback_error(
            "10",
            false,
            "job 101560010340 (`docs`): Not Found (HTTP 404)",
        )
        .with_log_fallback_error(
            "10",
            true,
            "job 101560010340 (`docs`): Not Found (HTTP 404)",
        )
        .with_job_log_fallback(
            "11",
            false,
            &job_log(),
            vec![json!({"job_id": 202, "name": "build", "conclusion": "failure"})],
        );

    let evidence = collect(&queries, &input()).expect("collect");

    // The run whose fallback recovered nothing carries no evidence and is not
    // investigated; it still must not silence the run beside it.
    let incomplete = failure_by_id(&evidence, 10).expect("the red run stays visible");
    assert_eq!(incomplete["investigated"], json!(false));
    assert_eq!(incomplete["log_excerpt"], json!(""));
    let complete = failure_by_id(&evidence, 11).expect("run 11 is still a current failure");
    assert_eq!(complete["investigated"], json!(true));
    assert_eq!(complete["log_source"], json!("job_api_log"));
    assert_eq!(complete["actual_checkout_shas"], json!([CHECKOUT]));
    assert_eq!(
        evidence["summary"]["investigated_failure_run_ids"],
        json!([11])
    );
    assert!(
        evidence["retryable_errors"]
            .as_array()
            .expect("retryable errors")
            .iter()
            .all(|error| error["run_id"] == json!(10)),
        "run 11's evidence is complete, so it owns no error: {}",
        evidence["retryable_errors"]
    );
}

fn two_job_queries(reverse: bool) -> FakeQueries {
    let mut jobs = vec![failed_job(201, "Clippy"), failed_job(202, "Coverage")];
    if reverse {
        jobs.reverse();
    }
    let mut queries = FakeQueries::authenticated()
        .with_head("topic", HEAD)
        .with_head("main", HEAD)
        .with_runs(vec![vec![failing_run(10)]])
        .with_run_view("10", json!({"failed_jobs": jobs}));
    for (job, sha, diagnostic) in [
        (201, CHECKOUT, "error: unused import"),
        (202, HEAD, "test output_goldens FAILED"),
    ] {
        queries.job_logs.insert(
            ("10".to_string(), job, false),
            format!("HEAD is now at {sha}\n{diagnostic}\n"),
        );
    }
    queries
}

#[test]
fn different_jobs_keep_their_own_diagnostic_and_checkout_in_either_order() {
    for reverse in [false, true] {
        let evidence = collect(&two_job_queries(reverse), &input()).expect("collect");
        let jobs = evidence["current_failures"].as_array().expect("jobs");
        assert_eq!(jobs.len(), 2);
        for (job, sha, diagnostic, excluded) in [
            (&jobs[0], CHECKOUT, "unused import", "output_goldens"),
            (&jobs[1], HEAD, "output_goldens", "unused import"),
        ] {
            assert_eq!(job["evidence_state"], "complete");
            assert_eq!(job["actual_checkout_shas"], json!([sha]));
            assert_eq!(
                job["checkout_identity"]["provenance"]["job_id"],
                job["job_id"]
            );
            assert_eq!(job["log_job_id"], job["job_id"]);
            let log = job["log_excerpt"].as_str().expect("log");
            assert!(log.contains(diagnostic));
            assert!(!log.contains(excluded));
        }
        assert_eq!(evidence["truncation"]["job_log_reads"], 2);
    }
}

#[test]
fn a_fallback_for_only_one_job_never_supplies_its_siblings_evidence() {
    let queries = two_job_queries(false).with_job_log_fallback(
        "10",
        false,
        &job_log(),
        vec![json!({"job_id": 202, "name": "Coverage"})],
    );
    let evidence = collect(&queries, &input()).expect("collect");
    let jobs = &evidence["current_failures"];
    assert_eq!(jobs[0]["job_id"], 201);
    assert_eq!(jobs[0]["evidence_state"], "deferred");
    assert!(jobs[0].get("log_excerpt").is_none());
    assert_eq!(jobs[0]["actual_checkout_shas"], json!([]));
    assert_eq!(jobs[1]["evidence_state"], "complete");
    assert_eq!(jobs[1]["actual_checkout_shas"], json!([CHECKOUT]));
    assert!(
        evidence["retryable_errors"]
            .as_array()
            .expect("errors")
            .iter()
            .all(|error| error["job_id"] == 201)
    );
}

#[test]
fn job_budget_rotates_without_swapping_evidence_and_truncation_is_deferred() {
    for cursor in 0..2 {
        let evidence = collect(&two_job_queries(false), &json!({
            "integration_branch": "topic", "max_job_log_reads": 1, "investigation_cursor": cursor,
        })).expect("collect");
        assert_eq!(evidence["truncation"]["job_log_reads"], 1);
        let jobs = evidence["current_failures"].as_array().expect("jobs");
        assert_eq!(jobs[cursor]["evidence_state"], "complete");
        assert_eq!(jobs[1 - cursor]["evidence_state"], "deferred");
        assert_eq!(
            evidence["retryable_errors"][0]["operation"],
            "job_log_budget"
        );
    }
    let mut queries = two_job_queries(false);
    queries.job_logs.insert(
        ("10".to_string(), 201, false),
        format!("{}\n{}", job_log(), "noise\n".repeat(100)),
    );
    let evidence = collect(
        &queries,
        &json!({"integration_branch": "topic", "log_max_bytes": 128}),
    )
    .expect("collect");
    assert_eq!(evidence["current_failures"][0]["log_truncated"], true);
    assert_eq!(
        evidence["current_failures"][0]["evidence_state"],
        "deferred"
    );
    assert_eq!(
        evidence["current_failures"][1]["evidence_state"],
        "complete"
    );
}

#[test]
fn checkout_fallback_is_bound_to_its_job_and_missing_logs_leave_siblings_complete() {
    let mut queries = two_job_queries(false);
    queries.job_logs.insert(
        ("10".to_string(), 201, false),
        "error: unused import\n".to_string(),
    );
    queries.job_logs.insert(
        ("10".to_string(), 202, false),
        "test output_goldens FAILED\n".to_string(),
    );
    let queries =
        queries.with_job_log_fallback("10", true, &job_log(), vec![json!({"job_id": 202})]);
    let evidence = collect(&queries, &input()).expect("collect");
    let jobs = &evidence["current_failures"];
    assert_eq!(jobs[0]["evidence_state"], "deferred");
    assert_eq!(jobs[0]["actual_checkout_shas"], json!([]));
    assert_eq!(jobs[1]["evidence_state"], "complete");
    assert_eq!(jobs[1]["checkout_identity"]["provenance"]["job_id"], 202);
    assert_eq!(jobs[1]["actual_checkout_shas"], json!([CHECKOUT]));
    assert_ne!(jobs[1]["actual_checkout_shas"], json!([HEAD]));
    assert_eq!(
        evidence["retryable_errors"][0]["operation"],
        "checkout_job_identity"
    );

    let mut queries = two_job_queries(false);
    queries.job_logs.remove(&("10".to_string(), 201, false));
    let evidence = collect(&queries, &input()).expect("collect");
    assert_eq!(
        evidence["current_failures"][0]["evidence_state"],
        "deferred"
    );
    assert_eq!(
        evidence["current_failures"][1]["evidence_state"],
        "complete"
    );
    assert!(
        evidence["retryable_errors"]
            .as_array()
            .expect("errors")
            .iter()
            .all(|error| error["job_id"] == 201)
    );
}

#[test]
fn long_job_logs_select_only_the_bound_complete_command_and_keep_display_metadata() {
    for reverse in [false, true] {
        let mut queries = two_job_queries(reverse);
        for (job, name, diagnostic) in [
            (201, "Clippy", "error: unused import `wrong_import`"),
            (
                202,
                "Coverage",
                "error[E0063]: missing field `owner_machine_id`",
            ),
        ] {
            let prefix = format!("{name}\t{name}\t2026-09-07T20:52:05Z ");
            let unit = format!(
                "{prefix}##[group]Run cargo test\n{prefix}##[endgroup]\n{prefix}{diagnostic}\n{prefix}  --> src/lib.rs:7:1\n{prefix}##[error]Process completed with exit code 101.\n"
            );
            queries.job_logs.insert(
                ("10".to_string(), job, false),
                format!(
                    "HEAD is now at {CHECKOUT}\n{}{}{}",
                    "unrelated setup error: setup_probe\n".repeat(1000),
                    unit,
                    "cleanup output\n".repeat(3000)
                ),
            );
        }
        let evidence = collect(&queries, &input()).expect("collect");
        let jobs = evidence["current_failures"].as_array().expect("jobs");
        assert_eq!(jobs.len(), 2);
        for (index, expected, excluded) in [
            (0, "wrong_import", "owner_machine_id"),
            (1, "owner_machine_id", "wrong_import"),
        ] {
            let job = &jobs[index];
            assert_eq!(job["evidence_state"], "complete");
            assert_eq!(job["log_truncated"], true);
            assert_eq!(job["actual_checkout_shas"], json!([CHECKOUT]));
            assert_eq!(job["diagnostic_unit"]["job_id"], job["job_id"]);
            let text = job["diagnostic_unit"]["text"]
                .as_str()
                .expect("selected evidence");
            assert!(text.contains(expected));
            assert!(!text.contains(excluded));
            assert!(!text.contains("setup_probe"));
            assert!(!text.contains("cleanup"));
        }
        assert_eq!(evidence["retryable_errors"], json!([]));
    }
}

#[test]
fn long_fallback_unit_requires_one_failed_step_and_complete_source() {
    let raw = format!(
        "{}\n{}{}{}",
        job_log()
            .split("2026-09-06T21:28")
            .next()
            .expect("checkout setup"),
        "setup output\n".repeat(2000),
        "2026-09-07T20:52:05Z ##[group]Run cargo doc\n\
         2026-09-07T20:52:05Z ##[endgroup]\n\
         2026-09-07T20:52:23Z error: private item `reject_root_override`\n\
         2026-09-07T20:52:31Z ##[error]Process completed with exit code 101.\n",
        "cleanup output\n".repeat(2000)
    );
    for defect in ["none", "steps", "source", "missing_end"] {
        let mut job = failed_job(101560010340, "docs");
        if defect == "steps" {
            job["failed_steps"] = json!([{"name": "A"}, {"name": "B"}]);
        }
        let log = match defect {
            "source" => format!("{raw}##[warning]Log output was truncated\n"),
            "missing_end" => raw.replace(
                "##[error]Process completed with exit code 101.",
                "lost completion",
            ),
            _ => raw.clone(),
        };
        let queries = FakeQueries::authenticated()
            .with_head("topic", HEAD)
            .with_head("main", HEAD)
            .with_runs(vec![vec![failing_run(10)]])
            .with_run_view("10", json!({"failed_jobs": [job]}))
            .with_job_log_fallback("10", false, &log, vec![source_job()]);
        let evidence = collect(&queries, &input()).expect("collect");
        let finding = failure_by_id(&evidence, 10).expect("finding");
        assert_eq!(finding["log_truncated"], true);
        assert_eq!(
            finding["investigated"],
            defect == "none",
            "{defect}: {finding}"
        );
        if defect == "none" {
            assert_eq!(finding["diagnostic_unit"]["job_id"], 101560010340_u64);
            assert_eq!(finding["diagnostic_unit"]["step"], "docs");
            assert!(
                finding["diagnostic_unit"]["text"]
                    .as_str()
                    .expect("unit")
                    .contains("reject_root_override")
            );
        }
    }
}

fn oversized_failure_queries(raw: &str) -> FakeQueries {
    let run_url = "https://github.com/constellation-works/orbit/actions/runs/34165795036";
    let mut job = failed_job(101876457414, "Check / Clippy / Test");
    job["url"] = json!(format!("{run_url}/job/101876457414"));
    job["failed_steps"] = json!([{"name": "Run CI guardrails", "conclusion": "failure"}]);
    let mut source_run = run(
        34165795036,
        "CI",
        "3ffa0fd3aaa535867c9e060c7ec600a33d7f2be6",
        "completed",
        Some("failure"),
        "2026-09-07T22:18:00Z",
    );
    source_run["url"] = json!(run_url);
    source_run["head_branch"] = json!("agent-main");
    let mut queries = FakeQueries::authenticated()
        .with_head("agent-main", "3ffa0fd3aaa535867c9e060c7ec600a33d7f2be6")
        .with_head("main", HEAD)
        .with_runs(vec![vec![source_run]])
        .with_run_view("34165795036", json!({"failed_jobs": [job.clone()]}))
        .with_job_log_fallback("34165795036", false, raw, vec![job]);
    queries.repo["full_name"] = json!("constellation-works/orbit");
    queries
}

fn check_oversized_finding(evidence: &Value) {
    let finding = failure_by_id(evidence, 34165795036).expect("exact run");
    assert_eq!(
        finding["investigated"], true,
        "{}",
        evidence["retryable_errors"]
    );
    assert_eq!(finding["log_source_complete"], true);
    assert_eq!(finding["log_truncated"], true);
    assert_eq!(
        finding["actual_checkout_shas"],
        json!(["3ffa0fd3aaa535867c9e060c7ec600a33d7f2be6"])
    );
    let unit = &finding["diagnostic_unit"];
    assert_eq!(unit["kind"], "runner_failure_regions");
    assert_eq!(unit["complete"], false);
    assert_eq!(unit["job_id"], 101876457414_u64);
    assert_eq!(unit["step"], "Run CI guardrails");
    let text = unit["text"].as_str().expect("bounded evidence");
    assert!(text.contains("plain_and_json_forms_match_their_goldens"));
    assert!(text.contains("output_goldens.rs:321:5"));
    assert!(text.contains("tool_list.plain.txt"));
    assert!(text.contains("exit code 100"));
    assert_eq!(evidence["retryable_errors"], json!([]));
}

#[test]
fn oversized_failure_regions_reach_collection_with_exact_job_step_and_checkout() {
    let raw = format!(
        "[command]/usr/bin/git log -1 --format=%H\n3ffa0fd3aaa535867c9e060c7ec600a33d7f2be6\n\
         ##[group]Run make ci\n{}\
         thread 'plain_and_json_forms_match_their_goldens' panicked at crates/orbit-cli/tests/output_goldens.rs:321:5:\n\
         assertion `left == right` failed: tool_list.plain.txt golden drift\n\
         left: {}\nright: expected\n\
         ##[error]Process completed with exit code 100.\n",
        "PASS ordinary_test\n".repeat(20_000),
        "large assertion ".repeat(10_000)
    );
    let evidence = collect(
        &oversized_failure_queries(&raw),
        &json!({"integration_branch": "agent-main"}),
    )
    .expect("collect");
    check_oversized_finding(&evidence);
}

/// Explicit, offline replay of the task's attached immutable runner bytes.
/// Export this production collection snapshot for the disposable filing test.
#[test]
#[ignore = "requires ORBIT_CI_REPLAY_LOG and ORBIT_CI_REPLAY_OUTPUT"]
fn replay_exact_guardrail_job_through_collection() {
    let path = std::env::var("ORBIT_CI_REPLAY_LOG").expect("attached source log path");
    let raw = std::fs::read_to_string(path).expect("read attached source log");
    let evidence = collect(
        &oversized_failure_queries(&raw),
        &json!({"integration_branch": "agent-main"}),
    )
    .expect("collect replay");
    check_oversized_finding(&evidence);
    let output = std::env::var("ORBIT_CI_REPLAY_OUTPUT").expect("snapshot output path");
    std::fs::write(
        output,
        serde_json::to_vec_pretty(&evidence).expect("snapshot JSON"),
    )
    .expect("write replay evidence");
}

#[test]
fn oversized_primary_regions_preserve_columns_and_reject_wrong_or_ambiguous_steps() {
    let raw = format!(
        "[command]/usr/bin/git log -1 --format=%H\n3ffa0fd3aaa535867c9e060c7ec600a33d7f2be6\n\
         ##[group]Run make ci\n{}\
         thread 'golden_failure' panicked at tests/golden.rs:1:1:\n\
         assertion failed: golden drift\n##[error]Process completed with exit code 100.\n",
        "PASS test\n".repeat(40_000)
    );
    for fault in ["none", "job", "step", "ambiguous"] {
        let mut queries = oversized_failure_queries(&raw);
        queries.job_log_fallbacks.clear();
        let job_name = if fault == "job" {
            "Other job"
        } else {
            "Check / Clippy / Test"
        };
        let step = if fault == "step" {
            "Other step"
        } else {
            "Run CI guardrails"
        };
        let primary = raw
            .lines()
            .map(|line| format!("{job_name}\t{step}\t{line}\n"))
            .collect();
        queries
            .job_logs
            .insert(("34165795036".to_string(), 101876457414, false), primary);
        if fault == "ambiguous" {
            queries.run_views.get_mut("34165795036").expect("view")["failed_jobs"][0]["failed_steps"] =
                json!([{"name": step}, {"name": "Other step"}]);
        }
        let evidence =
            collect(&queries, &json!({"integration_branch": "agent-main"})).expect("collect");
        let finding = failure_by_id(&evidence, 34165795036).expect("finding");
        assert_eq!(
            finding["investigated"],
            fault == "none",
            "{fault}: {}",
            evidence["retryable_errors"]
        );
        if fault == "none" {
            assert_eq!(finding["diagnostic_unit"]["kind"], "runner_failure_regions");
            assert_eq!(finding["diagnostic_unit"]["job_id"], 101876457414_u64);
            assert!(
                finding["diagnostic_unit"]["text"]
                    .as_str()
                    .expect("text")
                    .contains("command bytes omitted")
            );
        } else {
            assert!(finding["diagnostic_unit"].is_null());
        }
    }
}
