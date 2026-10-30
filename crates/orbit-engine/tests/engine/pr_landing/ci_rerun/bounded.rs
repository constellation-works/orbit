//! Oversized recovery evidence through the public adapter and completion.
use super::*;

// Reproduce ORB-19929 through the actual streaming reader, adapter and owner:
// the source is complete while both displays omit bytes. The root command
// itself exceeds the complete-unit cap; the waiter's small command does not.
pub(super) fn bounded_logs(fx: &Fixture, raw: bool) {
    let path = fx.forge.join("retry-config.json");
    let mut config: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    config["raw_fallback"] = json!(raw);
    fs::write(path, serde_json::to_vec(&config).unwrap()).unwrap();
    for (id, name, step, command) in [
        (1000, "test", "Tests", "echo tests"),
        (
            1001,
            "lint",
            "Wait",
            "python3 scripts/ci-await-jobs.py --job test",
        ),
    ] {
        let path = fx.forge.join(format!("retry-log-{id}"));
        let output = fs::read_to_string(&path).unwrap();
        let output = output
            .lines()
            .map(|line| line.splitn(3, '\t').nth(2).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        let prefix = if raw {
            "2026-10-29T10:00:30.0000000Z ".to_owned()
        } else {
            format!("{name}\t{step}\t")
        };
        let padding = format!("{prefix}{}\n", "ordinary output ".repeat(12));
        let shell = if id == 1001 {
            "/usr/bin/python3 -I {0}"
        } else {
            "/usr/bin/bash -e {0}"
        };
        let text = format!(
            "{}{prefix}##[group]Run {command}\n{prefix}{command}\n{prefix}shell: {shell}\n{prefix}##[endgroup]\n{}{prefix}{}\n{prefix}##[error]Process completed with exit code 1.\n",
            padding.repeat(800),
            padding.repeat(if id == 1000 { 6500 } else { 35 }),
            output.replace('\n', &format!("\n{prefix}")),
        );
        fs::write(path, text).unwrap();
    }
}

#[test]
fn bounded_dependency_failure_authenticates_and_reruns_once() {
    isolated(
        "ci_rerun::bounded::bounded_dependency_failure_authenticates_and_reruns_once",
        |sandbox| {
            for raw in [true, false] {
                in_fresh_row(sandbox, |row| {
                    let (fx, host, request) = fixture(row);
                    bounded_logs(&fx, raw);
                    for id in [1000, 1001] {
                        let read = run_private_vcs_operation("ci.job_log", &json!({"workspace_path":fx.repo,"run_id":"700","job_id":id.to_string()})).unwrap();
                        assert_eq!(read["truncated"], true);
                        assert_eq!(read["source_complete"], true);
                        assert_eq!(read["source"], if raw { "job_api_log" } else { "run_log" });
                        if id == 1000 {
                            assert_eq!(read["failure_regions"]["command_complete"], true);
                            assert_eq!(read["failure_regions"]["selection_complete"], true);
                        } else {
                            assert!(read["diagnostic"].is_string());
                        }
                    }
                    let plan = run_private_vcs_operation("ci.rerun_plan", &request).expect(
                        "complete bounded evidence must cross the previously held boundary",
                    );
                    assert_eq!(plan["plan"]["failed_jobs"].as_array().unwrap().len(), 2);
                    let logs = plan["plan"]["authentication"]["logs"].as_array().unwrap();
                    assert_eq!(logs[0]["proof"]["kind"], "selected_failure_regions");
                    assert_eq!(logs[1]["proof"]["kind"], "complete_command");
                    for log in logs {
                        assert_eq!(log["proof"]["display_truncated"], true);
                        assert_eq!(log["proof"]["source_complete"], true);
                        assert_eq!(
                            log["proof"]["binding"]["kind"],
                            if raw {
                                "trusted_workflow_command"
                            } else {
                                "provider_columns"
                            }
                        );
                    }
                    let mut input = fx.complete_ungated_input();
                    input["auto_fix"] = json!(true);
                    input["completion"] = json!("review");
                    let result = complete_with_logical_clock(&host, &input);
                    assert!(result.is_ok(), "raw={raw}: {result:?}");
                    assert_eq!(
                        fx.forge_lines("retry-sends"),
                        vec!["run rerun 700 --failed"]
                    );
                    assert_eq!(
                        host.history(TASK_ID)
                            .iter()
                            .filter(|entry| entry.event == CI_RERUN_RESERVED_EVENT)
                            .count(),
                        1
                    );
                    assert!(pr_auto_fix_attempts(&host.history(TASK_ID), "42").is_empty());
                });
            }
        },
    );
}

#[test]
fn bounded_retry_keeps_refusal_boundaries() {
    isolated(
        "ci_rerun::bounded::bounded_retry_keeps_refusal_boundaries",
        |sandbox| {
            for raw in [true, false] {
                for case in [
                    "source_incomplete",
                    "selection_incomplete",
                    "duplicate_record",
                    "omitted_record",
                    "wrong_command",
                    "wrong_shell",
                    "earlier_command",
                    "ambiguous_time",
                    "missing_time",
                    "wrong_columns",
                    "multiple_steps",
                    "unknown_step",
                    "unknown_outcome",
                    "fallback_source",
                    "ambiguous_source",
                    "duplicate_script",
                ] {
                    // Column provenance needs no raw workflow-command mapping.
                    if !raw
                        && matches!(
                            case,
                            "fallback_source"
                                | "ambiguous_source"
                                | "duplicate_script"
                                | "wrong_command"
                                | "wrong_shell"
                                | "earlier_command"
                                | "ambiguous_time"
                                | "missing_time"
                        )
                    {
                        continue;
                    }
                    in_fresh_row(sandbox, |row| {
                        let source = match case {
                        "ambiguous_source" => WORKFLOW.replace("  independent:\n    name: independent", "  independent:\n    name: test"),
                        "duplicate_script" => WORKFLOW.replace("        run: echo tests", "        run: echo tests\n      - name: Other tests\n        run: echo tests"),
                        _ => WORKFLOW.to_string(),
                    };
                        let (fx, host, request) = fixture_with_source(row, &source);
                        bounded_logs(&fx, raw);
                        let root = fx.forge.join("retry-log-1000");
                        let waiter = fx.forge.join("retry-log-1001");
                        let prefix = if raw {
                            "2026-10-29T10:00:30.0000000Z "
                        } else {
                            "test\tTests\t"
                        };
                        let waiter_prefix = if raw { prefix } else { "lint\tWait\t" };
                        let path = fx.forge.join("retry-config.json");
                        let mut config: Value =
                            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                        match case {
                            "source_incomplete" => {
                                let old = fs::read_to_string(&root).unwrap();
                                fs::write(
                                    &root,
                                    format!("{old}{prefix}##[warning]Log output truncated\n"),
                                )
                                .unwrap();
                            }
                            "selection_incomplete" => {
                                let old = fs::read_to_string(&root).unwrap();
                                let anchors = (0..129)
                                    .map(|i| format!("{prefix}FAIL [0.001s] engine::case_{i}\n"))
                                    .collect::<String>();
                                fs::write(
                                    &root,
                                    old.replace(
                                        "error: source failure",
                                        &format!("{anchors}{prefix}error: source failure"),
                                    ),
                                )
                                .unwrap();
                            }
                            "duplicate_record" | "omitted_record" => {
                                let old = fs::read_to_string(&waiter).unwrap();
                                let record = old
                                    .lines()
                                    .find(|line| line.contains("orbit-dependency-evidence:"))
                                    .unwrap();
                                let record = orbit_engine::ci_log_signature::log_payload(record);
                                let padding = if case == "omitted_record" {
                                    format!("{waiter_prefix}{}\n", "ordinary output ".repeat(12))
                                        .repeat(1800)
                                } else {
                                    String::new()
                                };
                                fs::write(&waiter, old.replace("##[error]Process completed", &format!("{record}\n{padding}{waiter_prefix}error: wrapper failure\n{waiter_prefix}##[error]Process completed"))).unwrap();
                            }
                            "earlier_command" => {
                                let old = fs::read_to_string(&waiter).unwrap();
                                fs::write(&waiter, old.replace("T10:00:30.", "T10:00:05."))
                                    .unwrap();
                            }
                            "ambiguous_time" | "missing_time" => {
                                let mut steps = json!([
                                    {"name":"Check out producer","number":1,"status":"completed","conclusion":"success","started_at":"2026-10-29T10:00:00Z","completed_at":"2026-10-29T10:01:00Z"},
                                    {"name":"Wait","number":2,"status":"completed","conclusion":"failure","started_at":"2026-10-29T10:00:10Z","completed_at":"2026-10-29T10:00:50Z"},
                                    {"name":"Own check","number":3,"status":"completed","conclusion":"success"}
                                ]);
                                if case == "missing_time" {
                                    steps[1]["started_at"] = Value::Null;
                                }
                                config["job_overrides"]["1001"]["steps"] = steps;
                            }
                            "wrong_shell" => {
                                let old = fs::read_to_string(&waiter).unwrap();
                                fs::write(
                                    &waiter,
                                    old.replace(
                                        "shell: /usr/bin/python3 -I {0}",
                                        "shell: /bin/bash -e {0}",
                                    ),
                                )
                                .unwrap();
                            }
                            "wrong_command" => {
                                let old = fs::read_to_string(&root).unwrap();
                                fs::write(&root, old.replace("echo tests", "echo forged")).unwrap();
                            }
                            "wrong_columns" if !raw => {
                                let old = fs::read_to_string(&root).unwrap();
                                fs::write(&root, old.replace("test\tTests\t", "test\tOther\t"))
                                    .unwrap();
                            }
                            "wrong_columns" => {
                                let old = fs::read_to_string(&root).unwrap();
                                fs::write(
                                    &root,
                                    old.replace(
                                        "shell: /usr/bin/bash -e {0}",
                                        "not runner shell metadata",
                                    ),
                                )
                                .unwrap();
                            }
                            "multiple_steps" => config["own_step"] = json!(true),
                            "unknown_step" => {
                                config["job_overrides"]["1000"]["steps"] = json!([{"name":"UNKNOWN STEP","number":1,"status":"completed","conclusion":"failure"}])
                            }
                            "unknown_outcome" => {
                                config["job_overrides"]["1000"]["steps"] = json!([{"name":"Tests","number":1,"status":"completed","conclusion":"unknown"}])
                            }
                            "fallback_source" => config["fallback_name"] = json!(true),
                            "ambiguous_source" | "duplicate_script" => {}
                            _ => unreachable!(),
                        }
                        fs::write(path, serde_json::to_vec(&config).unwrap()).unwrap();
                        let result = run_private_vcs_operation("ci.rerun_plan", &request);
                        assert!(
                            result.is_err(),
                            "raw={raw} case={case}: refusal was not enforced"
                        );
                        assert!(fx.forge_lines("retry-sends").is_empty());
                        assert!(pr_auto_fix_attempts(&host.history(TASK_ID), "42").is_empty());
                    });
                }
            }
        },
    );
}

#[test]
fn bounded_nextest_retry_observes_results_and_never_resends_unknown_replies() {
    isolated(
        "ci_rerun::bounded::bounded_nextest_retry_observes_results_and_never_resends_unknown_replies",
        |sandbox| {
            for case in ["pass", "repeat", "different_signature", "unknown"] {
                in_fresh_row(sandbox, |row| {
                    let (fx, host, mut request) = fixture(row);
                    bounded_logs(&fx, true);
                    let path = fx.forge.join("retry-log-1000");
                    let old = fs::read_to_string(&path).unwrap();
                    let prefix = "2026-10-29T10:00:30.0000000Z ";
                    fs::write(
                        &path,
                        old.replace(
                            "error: source failure",
                            &format!(
                                "FAIL [0.001s] engine::regression\n{prefix}error: source failure"
                            ),
                        ),
                    )
                    .unwrap();
                    request["roots"][0]["signature"] = json!(
                        orbit_engine::ci_log_signature::error_signature(
                            &fs::read_to_string(&path).unwrap(),
                            "Tests"
                        )
                        .text
                    );
                    let plan = run_private_vcs_operation("ci.rerun_plan", &request).unwrap();
                    assert!(plan["plan"]["authentication"]["logs"][0]["proof"]["failure_regions"]["byte_ranges"].is_array());
                    fs::copy(&path, fx.forge.join("retry-log-1010")).unwrap();
                    fs::copy(
                        fx.forge.join("retry-log-1001"),
                        fx.forge.join("retry-log-1011"),
                    )
                    .unwrap();
                    let path = fx.forge.join("retry-config.json");
                    let mut config: Value =
                        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                    if matches!(case, "repeat" | "different_signature") {
                        config["repeat"] = json!(true);
                    }
                    if case == "different_signature" {
                        let path = fx.forge.join("retry-log-1010");
                        let old = fs::read_to_string(&path).unwrap();
                        fs::write(path, old.replace("engine::regression", "engine::different"))
                            .unwrap();
                    }
                    if case == "unknown" {
                        config["reply"] = json!("unknown");
                    }
                    fs::write(path, serde_json::to_vec(&config).unwrap()).unwrap();
                    let mut input = fx.complete_ungated_input();
                    input["auto_fix"] = json!(true);
                    input["completion"] = json!("review");
                    input["max_wait_seconds"] = json!(10);
                    let result = complete_with_logical_clock(&host, &input);
                    if case == "pass" {
                        assert!(result.is_ok(), "{result:?}");
                    } else if case == "repeat" {
                        let error = result.expect_err(case);
                        assert!(
                            orbit_types::workflow::is_pr_auto_fix_check_repair(&error.to_string()),
                            "{error}"
                        );
                        assert_eq!(pr_auto_fix_attempts(&host.history(TASK_ID), "42").len(), 1);
                    } else {
                        assert!(result.is_err(), "{case}: {result:?}");
                        assert!(pr_auto_fix_attempts(&host.history(TASK_ID), "42").is_empty());
                    }
                    if case == "unknown" {
                        assert!(complete_with_logical_clock(&host, &input).is_err());
                    }
                    assert_eq!(
                        fx.forge_lines("retry-sends"),
                        vec!["run rerun 700 --failed"]
                    );
                    let reserved = host
                        .history(TASK_ID)
                        .into_iter()
                        .filter(|entry| entry.event == CI_RERUN_RESERVED_EVENT)
                        .collect::<Vec<_>>();
                    assert_eq!(reserved.len(), 1);
                    assert!(
                        reserved[0]
                            .note
                            .as_deref()
                            .unwrap()
                            .contains("selected_failure_regions")
                    );
                    assert_eq!(plan["plan"]["run_attempt"], 1);
                });
            }
        },
    );
}

#[test]
fn bounded_dependency_cycle_cannot_cover_failed_siblings() {
    isolated(
        "ci_rerun::bounded::bounded_dependency_cycle_cannot_cover_failed_siblings",
        |sandbox| {
            for raw in [true, false] {
                in_fresh_row(sandbox, |row| {
                    let mut source: Value = serde_yaml::from_str(WORKFLOW).unwrap();
                    source["jobs"]["independent"] = source["jobs"]["waiter"].clone();
                    source["jobs"]["independent"]["name"] = json!("independent");
                    source["jobs"]["independent"]["steps"][1]["env"]["ORBIT_DEPENDENCY_JOB_NAME"] =
                        json!("independent");
                    let (fx, host, request) =
                        fixture_with_source(row, &serde_yaml::to_string(&source).unwrap());
                    bounded_logs(&fx, raw);
                    let path = fx.forge.join("retry-log-1001");
                    let old = fs::read_to_string(&path).unwrap();
                    let record = old
                        .lines()
                        .find_map(|line| {
                            line.split_once("orbit-dependency-evidence:")
                                .map(|(_, body)| body)
                        })
                        .unwrap();
                    let mut first: Value = serde_json::from_str(record).unwrap();
                    let mut second = first.clone();
                    first["dependencies"][0]["job_id"] = json!(1002);
                    first["dependencies"][0]["job_name"] = json!("independent");
                    second["source"]["job_id"] = json!(1002);
                    second["source"]["job_key"] = json!("independent");
                    second["source"]["job_name"] = json!("independent");
                    second["dependencies"][0]["job_id"] = json!(1001);
                    second["dependencies"][0]["job_name"] = json!("lint");
                    fs::write(&path, old.replace(record, &first.to_string())).unwrap();
                    fs::write(
                        fx.forge.join("retry-log-1002"),
                        old.replace(record, &second.to_string())
                            .replace("lint\tWait\t", "independent\tWait\t"),
                    )
                    .unwrap();
                    let path = fx.forge.join("retry-config.json");
                    let mut config: Value =
                        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                    config["candidate"] = json!(true);
                    config["job_overrides"]["1002"]["steps"] = json!([
                        {"name":"Check out producer","number":1,"status":"completed","conclusion":"success"},
                        {"name":"Wait","number":2,"status":"completed","conclusion":"failure"},
                        {"name":"Own check","number":3,"status":"completed","conclusion":"success"}
                    ]);
                    fs::write(path, serde_json::to_vec(&config).unwrap()).unwrap();
                    let error = run_private_vcs_operation("ci.rerun_plan", &request)
                        .expect_err("cyclic waiters cannot reach the eligible root");
                    assert!(
                        error
                            .to_string()
                            .contains("cyclic retry dependency closure"),
                        "{error}"
                    );
                    assert!(fx.forge_lines("retry-sends").is_empty());
                    assert!(pr_auto_fix_attempts(&host.history(TASK_ID), "42").is_empty());
                });
            }
        },
    );
}
