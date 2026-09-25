//! Evidence completeness at filing: deferred findings, query errors, job-scoped
//! evidence, compiler causes and failure regions.

use orbit_types::task::TaskStatus;
use serde_json::{Value, json};

use super::filing::{CHECKOUT, NEXT_HEAD, failure, file, file_error, filed_task_ids, snapshot};
use super::log_signature::{excerpt_block, signature_line};
use crate::adapter::engine_host::v2_host::test_support::runtime_with_workspace_layout;
use crate::application::task::TaskUpdateParams;

#[test]
fn query_error_prevents_filing_and_remains_retryable() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let mut missing_log = failure(10, "ci", "build", "Run CI guardrails", "", CHECKOUT);
    missing_log["log_excerpt"] = json!("");
    let mut evidence = snapshot(vec![missing_log]);
    evidence["query_errors"] = json!([
        {
            "query": "run_logs",
            "run_id": "10",
            "error": "HTTP 404: Not Found — logs for this run are no longer available"
        },
        {
            "query": "run_list",
            "branch": "other",
            "error": "unrelated list failure"
        }
    ]);

    let error = file_error(&runtime, json!({"ci_evidence": evidence}));
    assert!(error.contains("retryable_error"));
    assert!(error.contains("run_logs"));
    assert!(error.contains("logs for this run are no longer available"));
    assert!(error.contains("\"current_failure_run_ids\":[10]"));
    assert!(
        runtime
            .list_tasks_by_tags(&["ci-failure-sweep".to_string()])
            .expect("list tasks")
            .is_empty()
    );
}

/// One uninvestigated run, shaped as collection leaves it when the budget runs
/// out: a URL and a verdict, no job, step, or log.
fn deferred_failure(run_id: u64, workflow: &str, branch: &str) -> Value {
    let mut failure = failure(run_id, workflow, "", "", "", "");
    failure["investigated"] = json!(false);
    failure["failed_jobs"] = json!([]);
    failure["log_excerpt"] = json!("");
    failure["actual_checkout_shas"] = json!([]);
    failure["checkout_evidence"] = json!([]);
    failure["head_branch"] = json!(branch);
    failure["ref_kind"] = json!("pull_request");
    failure
}

fn budget_error(run_id: u64) -> Value {
    json!({
        "stage": "investigation",
        "operation": "investigation_budget",
        "run_id": run_id,
        "retryable": true,
        "message": "current failure was not investigated because max_investigated_runs was exhausted",
    })
}

/// The jrun-20260905-1932 regression: fourteen candidates, three of them fully
/// evidenced, and the other eleven starved of investigation budget. The three
/// complete findings are real, filable defects and must not be withheld
/// because their neighbours are incomplete.
#[test]
fn complete_findings_file_while_incomplete_ones_stay_deferred() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let mut current = vec![
        failure(
            33_986_585_197,
            "Platform",
            "macOS",
            "cargo test",
            "ci\tmacOS\t2026-09-05T19:20:00Z ##[error]linker command failed\n",
            CHECKOUT,
        ),
        failure(
            33_986_582_084,
            "Website",
            "build",
            "sync website",
            "ci\tbuild\t2026-09-05T19:19:00Z ##[error]sync command not found\n",
            CHECKOUT,
        ),
        failure(
            33_986_085_270,
            "Pi",
            "macOS",
            "cargo build",
            "ci\tmacOS\t2026-09-05T19:10:00Z ##[error]could not compile orbit-pi\n",
            CHECKOUT,
        ),
    ];
    let deferred_ids = (0..11_u64)
        .map(|index| 33_900_000_000 + index)
        .collect::<Vec<_>>();
    for run_id in &deferred_ids {
        current.push(deferred_failure(
            *run_id,
            "Platform",
            "orbit/ORB-11200-older",
        ));
    }
    let mut evidence = snapshot(current);
    evidence["retryable_errors"] = json!(
        deferred_ids
            .iter()
            .map(|run_id| budget_error(*run_id))
            .collect::<Vec<_>>()
    );

    let output = file(&runtime, json!({"ci_evidence": evidence}));

    assert_eq!(output["outcome"], json!("current_failures"));
    assert_eq!(output["filed_count"], json!(3), "{output}");
    assert_eq!(filed_task_ids(&output).len(), 3);
    // The durable outcome is the tasks themselves, not the report.
    assert_eq!(
        runtime
            .list_tasks_by_tags(&["ci-failure-sweep".to_string()])
            .expect("list tasks")
            .len(),
        3
    );

    let deferred = output["deferred"].as_array().expect("deferred array");
    assert_eq!(deferred.len(), 11);
    assert_eq!(deferred[0]["run_id"], json!(33_900_000_000_u64));
    assert_eq!(deferred[0]["investigated"], json!(false));
    assert_eq!(deferred[0]["retryable"], json!(true));
    assert_eq!(
        deferred[0]["reasons"][0]["operation"],
        json!("investigation_budget")
    );

    let audit = &output["audit"];
    assert_eq!(audit["current_failures"], json!(14));
    assert_eq!(audit["investigated_failures"], json!(3));
    assert_eq!(audit["tasks_created"], json!(3));
    assert_eq!(audit["deferred_failures"], json!(11));
    assert_eq!(audit["retryable_errors"], json!(11));
    assert_eq!(
        audit["deferred_failure_run_ids"]
            .as_array()
            .expect("deferred ids")
            .len(),
        11
    );
}

/// The boundary the partial path must not cross. A listing that failed may be
/// the one holding the newer run that would have superseded a finding, so a
/// snapshot-wide error still withholds everything — including findings that
/// look complete.
#[test]
fn a_snapshot_wide_discovery_error_still_withholds_a_complete_finding() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let mut evidence = snapshot(vec![failure(
        10,
        "ci",
        "build",
        "cargo build",
        "ci\tbuild\t2026-08-30T01:00:00Z ##[error]expected 3 arguments\n",
        CHECKOUT,
    )]);
    evidence["retryable_errors"] = json!([
        {
            "stage": "discovery",
            "operation": "run_list",
            "run_id": Value::Null,
            "retryable": true,
            "message": "HTTP 502: Bad Gateway",
        },
        budget_error(11),
    ]);

    let error = file_error(&runtime, json!({"ci_evidence": evidence}));

    assert!(error.contains("retryable_error"));
    assert!(error.contains("run_list"));
    // The run-scoped error travels with it, so one payload explains the sweep.
    assert!(error.contains("investigation_budget"));
    assert!(
        runtime
            .list_tasks_by_tags(&["ci-failure-sweep".to_string()])
            .expect("list tasks")
            .is_empty()
    );
}

/// A finding whose own run carries an error is not filed from partial
/// evidence: per-finding requirements are unchanged, and the gap is stated
/// rather than papered over.
#[test]
fn a_finding_whose_own_run_failed_a_query_is_deferred_not_filed_from_partial_evidence() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let mut evidence = snapshot(vec![
        failure(
            10,
            "ci",
            "build",
            "cargo build",
            "ci\tbuild\t2026-08-30T01:00:00Z ##[error]expected 3 arguments\n",
            CHECKOUT,
        ),
        failure(
            11,
            "Website",
            "deploy",
            "sync website",
            "ci\tdeploy\t2026-08-30T01:00:00Z ##[error]sync command not found\n",
            CHECKOUT,
        ),
    ]);
    evidence["retryable_errors"] = json!([
        {
            "stage": "registration",
            "operation": "checkout_evidence",
            "run_id": 11,
            "retryable": true,
            "message": "checkout evidence scan reached its hard limit; actual checkout identity is incomplete",
        }
    ]);

    let output = file(&runtime, json!({"ci_evidence": evidence}));

    assert_eq!(output["filed_count"], json!(1));
    assert_eq!(output["filed"][0]["workflow"], json!("ci"));
    let deferred = output["deferred"].as_array().expect("deferred array");
    assert_eq!(deferred.len(), 1);
    assert_eq!(deferred[0]["run_id"], json!(11));
    assert_eq!(
        deferred[0]["investigated"],
        json!(true),
        "the run was investigated; its evidence is what is incomplete"
    );
    assert_eq!(
        deferred[0]["reasons"][0]["operation"],
        json!("checkout_evidence")
    );
}

fn two_job_findings() -> Vec<Value> {
    let first = failure(
        10,
        "CI",
        "Clippy",
        "Run guardrails",
        "error: unused import",
        CHECKOUT,
    );
    let mut second = failure(
        10,
        "CI",
        "Coverage",
        "Collect coverage",
        "test output_goldens FAILED",
        NEXT_HEAD,
    );
    second["job_id"] = json!(920);
    second["log_job_id"] = json!(920);
    second["failed_jobs"][0]["job_id"] = json!(920);
    second["checkout_identity"]["provenance"]["job_id"] = json!(920);
    vec![first, second]
}

#[test]
fn two_failed_jobs_file_distinct_correct_findings_regardless_of_order() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let findings = two_job_findings();
    let first = file(&runtime, json!({"ci_evidence": snapshot(findings.clone())}));
    assert_eq!(first["filed_count"], 2);
    let ids = filed_task_ids(&first);
    let clippy = runtime.get_task(&ids[0]).expect("clippy task");
    let coverage = runtime.get_task(&ids[1]).expect("coverage task");
    assert!(clippy.title.contains("Clippy"));
    assert!(clippy.description.contains("unused import"));
    assert!(!clippy.description.contains("output_goldens"));
    assert!(coverage.title.contains("Coverage"));
    assert!(coverage.description.contains("output_goldens"));
    assert!(!coverage.description.contains("unused import"));
    assert_ne!(
        first["filed"][0]["failure_key"],
        first["filed"][1]["failure_key"]
    );
    assert_eq!(first["filed"][0]["tested_commit"], CHECKOUT);
    assert_eq!(first["filed"][1]["tested_commit"], NEXT_HEAD);
    let mut reversed = findings;
    reversed.reverse();
    let second = file(&runtime, json!({"ci_evidence": snapshot(reversed)}));
    assert_eq!(second["filed_count"], 0);
    assert_eq!(
        second["skipped_existing"].as_array().expect("skips").len(),
        2
    );
}

#[test]
fn one_jobs_retryable_error_defers_only_that_job() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let mut findings = two_job_findings();
    findings[0]["investigated"] = json!(false);
    let mut evidence = snapshot(findings);
    evidence["retryable_errors"] = json!([{
        "run_id": 10, "job_id": 910, "operation": "run_logs", "message": "job log unavailable",
    }]);
    let output = file(&runtime, json!({"ci_evidence": evidence}));
    assert_eq!(output["filed_count"], 1);
    assert_eq!(output["filed"][0]["job"], "Coverage");
    assert_eq!(output["deferred"][0]["job_id"], 910);
    assert_eq!(output["deferred"][0]["reasons"][0]["job_id"], 910);
    assert_eq!(output["deferred"].as_array().expect("deferred").len(), 1);
}

#[test]
fn unbound_legacy_and_incomplete_job_snapshots_require_recollection() {
    for defect in [
        "legacy",
        "fallback",
        "checkout",
        "truncated",
        "missing",
        "steps",
    ] {
        let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
        let mut findings = two_job_findings();
        findings.truncate(1);
        let mut evidence = snapshot(findings);
        let finding = &mut evidence["current_failures"][0];
        match defect {
            "fallback" => {
                finding["log_source"] = json!("job_api_log");
                finding["log_source_jobs"] = json!([{"job_id": 920}]);
            }
            "checkout" => finding["checkout_identity"]["provenance"]["job_id"] = json!(920),
            "truncated" => finding["log_truncated"] = json!(true),
            "missing" => finding["log_excerpt"] = json!(""),
            "steps" => {
                finding["failed_jobs"][0]["failed_steps"] = json!([{"name": "A"}, {"name": "B"}])
            }
            _ => evidence["schema_version"] = json!(1),
        }
        let error = file_error(&runtime, json!({"ci_evidence": evidence}));
        assert!(error.contains("job_evidence_identity"), "{defect}: {error}");
        assert!(
            runtime
                .list_tasks_by_tags(&["ci-failure-sweep".to_string()])
                .expect("tasks")
                .is_empty()
        );
    }
}

#[test]
fn legacy_multi_job_snapshot_cannot_label_coverage_log_as_clippy() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let findings = two_job_findings();
    let mut combined = findings[0].clone();
    combined["failed_jobs"] =
        json!([findings[0]["failed_jobs"][0], findings[1]["failed_jobs"][0],]);
    combined["log_excerpt"] = json!("Coverage\tCollect coverage\ttest output_goldens FAILED\n");
    let mut evidence = snapshot(vec![combined]);
    evidence["schema_version"] = json!(1);
    let error = file_error(&runtime, json!({"ci_evidence": evidence}));
    assert!(error.contains("legacy run-scoped evidence"), "{error}");
    assert!(
        runtime
            .list_tasks_by_tags(&["ci-failure-sweep".to_string()])
            .expect("tasks")
            .is_empty()
    );
}

#[test]
fn complete_units_from_long_logs_file_and_dedupe_without_using_display_noise() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let mut findings = two_job_findings();
    for finding in &mut findings {
        let text = finding["log_excerpt"]
            .as_str()
            .expect("diagnostic")
            .to_string();
        finding["diagnostic_unit"] = json!({"kind": "runner_command", "complete": true,
            "job_id": finding["job_id"], "step": finding["failed_jobs"][0]["failed_steps"][0]["name"],
            "text": text});
        finding["log_excerpt"] = json!("setup error: unrelated_setup\n[... omitted ...]\ncleanup");
        finding["log_truncated"] = json!(true);
        finding["log_source_complete"] = json!(true);
    }
    let first = file(&runtime, json!({"ci_evidence": snapshot(findings.clone())}));
    assert_eq!(first["filed_count"], 2);
    let ids = filed_task_ids(&first);
    for (id, expected) in ids.iter().zip(["unused import", "output_goldens"]) {
        let task = runtime.get_task(id).expect("task");
        assert!(task.description.contains(expected));
        assert!(
            task.description
                .contains("collection display was truncated")
        );
        assert!(!task.description.contains("unrelated_setup"));
    }
    findings.reverse();
    let second = file(&runtime, json!({"ci_evidence": snapshot(findings)}));
    assert_eq!(second["filed_count"], 0);
    assert_eq!(
        second["skipped_existing"]
            .as_array()
            .expect("deduped")
            .len(),
        2
    );
}

#[test]
fn incomplete_or_foreign_units_cannot_override_truncated_display() {
    for fault in ["job", "step", "incomplete", "source", "generic", "oversize"] {
        let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
        let mut findings = two_job_findings();
        findings.truncate(1);
        let finding = &mut findings[0];
        finding["log_truncated"] = json!(true);
        finding["diagnostic_unit"] = json!({"kind": "runner_command", "complete": true,
            "job_id": finding["job_id"], "step": finding["failed_jobs"][0]["failed_steps"][0]["name"],
            "text": "error: concrete diagnostic"});
        match fault {
            "job" => finding["diagnostic_unit"]["job_id"] = json!(999),
            "step" => finding["diagnostic_unit"]["step"] = json!("Other step"),
            "incomplete" => finding["diagnostic_unit"]["complete"] = json!(false),
            "source" => finding["log_source_complete"] = json!(false),
            "generic" => {
                finding["diagnostic_unit"]["text"] =
                    json!("##[error]Process completed with exit code 101.")
            }
            _ => finding["diagnostic_unit"]["text"] = json!("x".repeat(262_145)),
        }
        let error = file_error(&runtime, json!({"ci_evidence": snapshot(findings)}));
        assert!(error.contains("job_evidence_identity"), "{fault}: {error}");
    }
}

pub(super) fn compiler_findings() -> Vec<Value> {
    ["macOS", "Clippy", "Coverage"]
        .into_iter()
        .enumerate()
        .map(|(index, job)| {
            let log = format!(
                concat!(
                    "##[group]Run cargo check\n",
                    "    Compiling thiserror v2.0.17\n",
                    "    Checking error_stack v1.0.0\n",
                    "error: process didn't exit successfully: `rustc {}` (exit status: 1)\n",
                    "\x1b[1;31merror[E0062]\x1b[0m: field `owner_machine_id` specified more than once\n",
                    "  --> crates/orbit-core/src/ci_sweep.rs:275:13\n",
                    "   |\n275 | owner_machine_id: None,\n",
                    "   | ^^^^^^^^^^^^^^^^ used more than once\n",
                    "error: could not compile `orbit-core` due to 1 previous error\n",
                    "##[error]Process completed with exit code 101.\n",
                ),
                "--extern error_helper=/tmp/build/é ".repeat(1500)
            );
            let mut finding = failure(70 + index as u64, "CI", job, job, &log, CHECKOUT);
            finding["diagnostic_unit"] = json!({"kind": "runner_command", "complete": true,
                "job_id": finding["job_id"], "step": job, "text": log});
            finding["log_source_complete"] = json!(true);
            finding["log_truncated"] = json!(true);
            finding["log_excerpt"] = json!("setup error: unrelated display noise");
            finding
        })
        .collect()
}

#[test]
fn compiler_cause_consolidates_jobs_and_keeps_actionable_excerpt() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let mut findings = compiler_findings();
    let output = file(&runtime, json!({"ci_evidence": snapshot(findings.clone())}));
    assert_eq!(output["filed_count"], 1, "{output}");
    assert_eq!(output["clusters"], 1);
    let id = filed_task_ids(&output).remove(0);
    let task = runtime.get_task(&id).expect("compiler owner");
    let signature = signature_line(&task.description);
    assert!(
        signature.contains("error[e0062]: field `owner_machine_id`"),
        "{signature}"
    );
    assert!(!signature.contains('\x1b'));
    let excerpt = excerpt_block(&task.description);
    assert!(excerpt.contains("error[E0062]"));
    assert!(excerpt.contains("ci_sweep.rs:275:13"));
    assert!(excerpt.len() < 4_300, "{}", excerpt.len());
    for finding in &findings {
        assert!(
            task.description
                .contains(finding["failed_jobs"][0]["name"].as_str().expect("name"))
        );
        assert!(task.description.contains(&finding["job_id"].to_string()));
        assert!(task.description.contains(&finding["run_id"].to_string()));
    }
    assert!(task.description.contains(CHECKOUT));
    findings.reverse();
    let repeated = file(&runtime, json!({"ci_evidence": snapshot(findings)}));
    assert_eq!(repeated["filed_count"], 0);
    assert_eq!(repeated["skipped_existing"][0]["task_id"], id);
}

#[test]
fn compiler_causes_with_shared_command_and_location_remain_separate() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let mut findings = compiler_findings();
    for (index, finding) in findings.iter_mut().enumerate() {
        let text = finding["diagnostic_unit"]["text"].as_str().expect("log");
        finding["diagnostic_unit"]["text"] =
            json!(text.replace("owner_machine_id", &format!("field_{index}")));
    }
    let first = file(&runtime, json!({"ci_evidence": snapshot(findings.clone())}));
    assert_eq!(first["filed_count"], 3);
    findings.reverse();
    let repeated = file(&runtime, json!({"ci_evidence": snapshot(findings)}));
    assert_eq!(repeated["filed_count"], 0);
    let owners: std::collections::BTreeSet<_> = repeated["skipped_existing"]
        .as_array()
        .expect("skips")
        .iter()
        .map(|entry| entry["task_id"].as_str().expect("id"))
        .collect();
    assert_eq!(owners.len(), 3);
}

#[test]
fn compiler_legacy_keys_follow_rejected_owners_only_for_the_original_source() {
    use crate::application::task::TaskAddParams;

    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let findings = compiler_findings();
    let owner = runtime
        .add_task(TaskAddParams {
            title: "Repair the compiler initializer".to_string(),
            description: "Canonical repair explicitly named by the rejected observations."
                .to_string(),
            ..TaskAddParams::default()
        })
        .expect("owner");
    // Shipped workflow/job/step/first-marker digests for these exact logs.
    let keys = ["2c5df683bc1b014d", "c9f31b827d835692", "14ccd1ad54c810ec"];
    for (finding, key) in findings.iter().zip(keys) {
        let source = runtime
            .add_task(TaskAddParams {
                title: "Legacy compiler observation".to_string(),
                description: format!(
                    "run `{}`\nfailed job (id `{}`)\ncommit actually checked out: `{CHECKOUT}`",
                    finding["run_id"], finding["job_id"]
                ),
                tags: vec![format!("ci-failure:{key}")],
                ..TaskAddParams::default()
            })
            .expect("legacy observation");
        runtime
            .update_task(
                &source.id,
                TaskUpdateParams {
                    status: Some(TaskStatus::Rejected),
                    comment: Some(format!("Duplicate of {}", owner.id)),
                    ..TaskUpdateParams::default()
                },
            )
            .expect("rejected duplicate");
    }
    for finding in &findings {
        let output = file(
            &runtime,
            json!({"ci_evidence": snapshot(vec![finding.clone()])}),
        );
        assert_eq!(output["filed_count"], 0, "{output}");
        assert_eq!(output["skipped_existing"][0]["task_id"], owner.id);
        assert_eq!(
            output["skipped_existing"][0]["match_kind"],
            "confirmed_duplicate"
        );
    }
    let mut reversed = findings.clone();
    reversed.reverse();
    for current in [findings.clone(), reversed] {
        let output = file(&runtime, json!({"ci_evidence": snapshot(current)}));
        assert_eq!(output["filed_count"], 0);
        assert_eq!(output["skipped_existing"][0]["task_id"], owner.id);
        assert_eq!(
            output["skipped_existing"][0]["sources"]
                .as_array()
                .expect("sources")
                .len(),
            3
        );
    }
    // The old chatter key recurs, but a new run is not the original evidence.
    let mut later = findings[0].clone();
    later["run_id"] = json!(99);
    let output = file(&runtime, json!({"ci_evidence": snapshot(vec![later])}));
    assert_eq!(output["filed_count"], 1);
}

#[test]
fn compiler_proof_preserves_case_coordinates_checkout_and_secondary_errors() {
    for difference in [
        "case",
        "location",
        "checkout",
        "secondary",
        "missing_location",
    ] {
        let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
        let mut findings = compiler_findings();
        findings.truncate(2);
        let text = findings[1]["diagnostic_unit"]["text"]
            .as_str()
            .expect("log");
        let changed = match difference {
            "case" => text.replace("owner_machine_id", "Owner_machine_id"),
            "location" => text.replace(":275:13", ":276:13"),
            "checkout" => text.to_string(),
            "secondary" => format!(
                "{text}\nerror[E0308]: mismatched types\n --> crates/orbit-core/src/ci_sweep.rs:275:13\n"
            ),
            _ => text.replace("  --> crates/orbit-core/src/ci_sweep.rs:275:13\n", ""),
        };
        findings[1]["diagnostic_unit"]["text"] = json!(changed);
        if difference == "checkout" {
            findings[1]["actual_checkout_shas"] = json!([NEXT_HEAD]);
        }
        let output = file(&runtime, json!({"ci_evidence": snapshot(findings)}));
        assert_eq!(output["filed_count"], 2, "{difference}: {output}");
    }
}

fn region_finding() -> Value {
    let raw = format!(
        "##[group]Run cargo nextest run\n{}\
         thread 'first_failure' panicked at tests/golden.rs:12:5:\n\
         assertion failed: tool_list.plain.txt golden drift\nleft: {}\nright: expected\n\
         thread 'second_failure' panicked at tests/other.rs:20:7:\n\
         assertion failed: second condition\n\
         ##[error]Process completed with exit code 100.\n",
        "PASS ordinary_test\n".repeat(20_000),
        "large assertion ".repeat(10_000)
    );
    let mut collector = orbit_tools::github_cli::StreamedLogCollector::new(128, 40);
    for chunk in raw.as_bytes().chunks(4096) {
        collector.push(chunk);
    }
    let log = collector.finish();
    let mut finding = failure(10, "CI", "Check", "Run tests", &log.text, CHECKOUT);
    let mut unit = log.failure_regions.expect("selected regions");
    unit["job_id"] = finding["job_id"].clone();
    unit["step"] = json!("Run tests");
    finding["diagnostic_unit"] = unit;
    finding["log_source_complete"] = json!(log.source_complete);
    finding["log_truncated"] = json!(log.truncated);
    finding
}

#[test]
fn oversized_regions_file_all_failures_and_explicit_omissions_then_dedupe() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let evidence = snapshot(vec![region_finding()]);
    let first = file(&runtime, json!({"ci_evidence": evidence}));
    assert_eq!(first["filed_count"], 1);
    let id = filed_task_ids(&first).remove(0);
    let task = runtime.get_task(&id).expect("task");
    for expected in [
        "first_failure",
        "second_failure",
        "golden.rs:12:5",
        "other.rs:20:7",
        "golden drift",
        "full command was not retained",
        "assertion payload bytes",
        "right: expected",
    ] {
        assert!(
            task.description.contains(expected),
            "missing {expected}: {}",
            task.description
        );
    }
    assert!(task.required_tools.is_empty());
    let second = file(&runtime, json!({"ci_evidence": evidence}));
    assert_eq!(second["filed_count"], 0);
    assert_eq!(second["skipped_existing"][0]["task_id"], id);
}

#[test]
fn failure_regions_reject_false_completeness_missing_accounting_and_foreign_identity() {
    for fault in [
        "complete",
        "command",
        "selection",
        "source",
        "job",
        "untruncated_foreign",
        "checkout",
        "step",
        "accounting",
        "assertions",
        "size",
    ] {
        let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
        let mut finding = region_finding();
        match fault {
            "complete" => finding["diagnostic_unit"]["complete"] = json!(true),
            "command" => finding["diagnostic_unit"]["command_complete"] = json!(false),
            "selection" => finding["diagnostic_unit"]["selection_complete"] = json!(false),
            "source" => finding["log_source_complete"] = json!(false),
            "job" => finding["diagnostic_unit"]["job_id"] = json!(920),
            "untruncated_foreign" => {
                finding["diagnostic_unit"]["job_id"] = json!(920);
                finding["log_truncated"] = json!(false);
            }
            "checkout" => finding["checkout_identity"]["provenance"]["job_id"] = json!(920),
            "step" => finding["diagnostic_unit"]["step"] = json!("Other"),
            "accounting" => finding["diagnostic_unit"]["omitted_bytes"] = json!(0),
            "assertions" => {
                finding["diagnostic_unit"]["assertion_payload_omitted_bytes"] = Value::Null
            }
            _ => finding["diagnostic_unit"]["returned_bytes"] = json!(1),
        }
        let error = file_error(&runtime, json!({"ci_evidence": snapshot(vec![finding])}));
        assert!(error.contains("job_evidence_identity"), "{fault}: {error}");
    }
}

/// Consumes the exact production snapshot exported by the engine replay.
/// Both filings use only this disposable runtime's registry and task store.
#[test]
#[ignore = "requires ORBIT_CI_REPLAY_OUTPUT and ORBIT_CI_REPLAY_REPORT"]
fn replay_exact_guardrail_snapshot_through_disposable_filing() {
    let (_root, runtime, _repo_root) = runtime_with_workspace_layout();
    let path = std::env::var("ORBIT_CI_REPLAY_OUTPUT").expect("collection snapshot path");
    let evidence: Value =
        serde_json::from_slice(&std::fs::read(path).expect("snapshot")).expect("JSON");
    let first = file(&runtime, json!({"ci_evidence": evidence}));
    assert_eq!(first["filed_count"], 1, "{first}");
    let id = filed_task_ids(&first).remove(0);
    let task = runtime.get_task(&id).expect("repair task");
    for expected in [
        "101876457414",
        "34165795036",
        "Check / Clippy / Test",
        "Run CI guardrails",
        "3ffa0fd3aaa535867c9e060c7ec600a33d7f2be6",
        "plain_and_json_forms_match_their_goldens",
        "output_goldens.rs:321:5",
        "tool_list.plain.txt",
        "full command was not retained",
        "assertion payload bytes",
    ] {
        assert!(
            task.description.contains(expected),
            "missing {expected}: {}",
            task.description
        );
    }
    assert!(
        !task
            .description
            .contains("eb26940c037ce255b6c28c0378c9276ab38cc75e")
    );
    assert!(task.required_tools.is_empty());
    let second = file(&runtime, json!({"ci_evidence": evidence}));
    assert_eq!(second["filed_count"], 0);
    assert_eq!(second["skipped_existing"][0]["task_id"], id);
    let report = json!({"first_filing": first, "repeat_filing": second, "offline_task_description": task.description, "required_tools": task.required_tools});
    std::fs::write(
        std::env::var("ORBIT_CI_REPLAY_REPORT").expect("report path"),
        serde_json::to_vec_pretty(&report).expect("report JSON"),
    )
    .expect("write replay report");
}
