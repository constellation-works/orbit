//! The rule-findings filer keeps one task per fingerprint: a new failure
//! files once, a repeat files nothing, changed measurements comment, a
//! pass comments exactly once, and the severity cap and floor hold.

use orbit_core::{OrbitRuntime, TaskStatus};
use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use serde_json::{Value, json};

fn fixture(config: &str) -> (tempfile::TempDir, OrbitRuntime) {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global_root = root.path().join("home/.orbit");
    let workspace_root = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global_root).unwrap();
    std::fs::create_dir_all(&workspace_root).unwrap();
    std::fs::write(global_root.join("config.toml"), config).unwrap();
    let runtime = OrbitRuntime::from_roots(&global_root, &workspace_root).unwrap();
    (root, runtime)
}

fn file(runtime: &OrbitRuntime, results: Value, extra: Value) -> Value {
    let mut input = json!({"results": results});
    if let Value::Object(extra) = extra {
        for (key, value) in extra {
            input[key] = value;
        }
    }
    runtime
        .run_deterministic(
            "file_rule_findings",
            &json!({}),
            &input,
            ToolContext::default(),
        )
        .unwrap()
}

fn document(results: Vec<Value>) -> Value {
    json!({
        "schema_version": 1, "surface": "cli", "commit": "abc123",
        "results": results,
    })
}

fn result(status: &str, severity: &str, fingerprint: &str, measurements: Value) -> Value {
    json!({
        "rule": "cli.example", "target": {"id": "orbit run", "argv": ["run"]},
        "status": status, "severity": severity,
        "measurements": measurements, "evidence": [], "fingerprint": fingerprint,
    })
}

fn failing_fingerprint_files_once_then_comments_on_change_and_pass() {
    let (_root, runtime) = fixture("");
    let failing = document(vec![result("fail", "high", "fp1", json!({"gap": 1}))]);
    let first = file(&runtime, failing.clone(), json!({}));
    assert_eq!(first["filed_count"], 1, "{first}");
    assert_eq!(first["counts"]["fail"], 1);
    let task_id = first["filed"][0]["task_id"].as_str().unwrap().to_string();
    let task = runtime.get_task(&task_id).unwrap();
    assert_eq!(task.status, TaskStatus::Proposed);
    assert_eq!(task.rule_fingerprint.as_deref(), Some("fp1"));
    assert!(
        task.description.contains("--rule cli.example"),
        "{}",
        task.description
    );
    assert!(task.tags.iter().any(|tag| tag == "ux-rule:cli.example"));

    // Same failure again: nothing new, no comment.
    let second = file(&runtime, failing, json!({}));
    assert_eq!(second["filed_count"], 0, "{second}");
    assert_eq!(second["commented"].as_array().unwrap().len(), 0);

    // Changed measurements: one comment, still proposed.
    let changed = document(vec![result("fail", "high", "fp1", json!({"gap": 2}))]);
    let third = file(&runtime, changed, json!({}));
    assert_eq!(third["commented"].as_array().unwrap().len(), 1, "{third}");

    // Passing: exactly one comment across two passing runs, no transition.
    let passing = document(vec![result("pass", "high", "fp1", json!({"gap": 0}))]);
    let fourth = file(&runtime, passing.clone(), json!({}));
    assert_eq!(fourth["commented"].as_array().unwrap().len(), 1, "{fourth}");
    let fifth = file(&runtime, passing, json!({}));
    assert_eq!(fifth["commented"].as_array().unwrap().len(), 0, "{fifth}");
    assert_eq!(
        runtime.get_task(&task_id).unwrap().status,
        TaskStatus::Proposed
    );
}

fn oscillating_measurements_comment_on_return_to_earlier_value() {
    let (_root, runtime) = fixture("");
    let first = file(
        &runtime,
        document(vec![result("fail", "high", "fp-osc", json!({"gap": 1}))]),
        json!({}),
    );
    assert_eq!(first["filed_count"], 1, "{first}");
    let changed = file(
        &runtime,
        document(vec![result("fail", "high", "fp-osc", json!({"gap": 2}))]),
        json!({}),
    );
    assert_eq!(
        changed["commented"].as_array().unwrap().len(),
        1,
        "{changed}"
    );
    assert_eq!(changed["commented"][0]["kind"], "measurements_changed");
    // Returning to gap 1 repeats the filing description, but the latest
    // recorded measurement was gap 2, so this must comment again.
    let back = file(
        &runtime,
        document(vec![result("fail", "high", "fp-osc", json!({"gap": 1}))]),
        json!({}),
    );
    assert_eq!(back["commented"].as_array().unwrap().len(), 1, "{back}");
    assert_eq!(back["commented"][0]["kind"], "measurements_changed");
}

fn fail_pass_fail_pass_records_second_pass_transition() {
    let (_root, runtime) = fixture("");
    let first = file(
        &runtime,
        document(vec![result("fail", "high", "fp-flap", json!({"gap": 1}))]),
        json!({}),
    );
    assert_eq!(first["filed_count"], 1, "{first}");
    let passing = document(vec![result("pass", "high", "fp-flap", json!({"gap": 0}))]);
    let second = file(&runtime, passing.clone(), json!({}));
    assert_eq!(second["commented"].as_array().unwrap().len(), 1, "{second}");
    assert_eq!(second["commented"][0]["kind"], "pass");
    // The second fail repeats the filing description, but the latest marker
    // is a pass, so it must record a fresh fail transition.
    let refail = file(
        &runtime,
        document(vec![result("fail", "high", "fp-flap", json!({"gap": 1}))]),
        json!({}),
    );
    assert_eq!(refail["commented"].as_array().unwrap().len(), 1, "{refail}");
    assert_eq!(refail["commented"][0]["kind"], "measurements_changed");
    // With the fresh fail marker recorded, the second pass must comment.
    let repass = file(&runtime, passing, json!({}));
    assert_eq!(repass["commented"].as_array().unwrap().len(), 1, "{repass}");
    assert_eq!(repass["commented"][0]["kind"], "pass");
}

fn cap_takes_highest_severity_first_and_floor_records_only() {
    let (_root, runtime) = fixture("[ux_rules]\nmax_new_tasks = 1\nfile_at = \"low\"\n");
    let results = document(vec![
        result("fail", "low", "fp-low", json!({})),
        result("fail", "critical", "fp-crit", json!({})),
        result("fail", "nit", "fp-nit", json!({})),
    ]);
    let outcome = file(&runtime, results, json!({}));
    assert_eq!(outcome["filed_count"], 1, "{outcome}");
    assert_eq!(outcome["filed"][0]["fingerprint"], "fp-crit");
    assert_eq!(outcome["skipped_over_cap"].as_array().unwrap().len(), 1);
    assert_eq!(outcome["recorded_only"].as_array().unwrap().len(), 1);
    assert_eq!(outcome["recorded_only"][0]["fingerprint"], "fp-nit");
}

fn blank_file_at_uses_configured_floor() {
    let (_root, runtime) = fixture("[ux_rules]\nfile_at = \"high\"\n");
    let outcome = file(
        &runtime,
        document(vec![result("fail", "medium", "fp-empty-floor", json!({}))]),
        json!({"file_at": ""}),
    );
    assert_eq!(outcome["file_at"], "high", "{outcome}");
    assert_eq!(outcome["filed_count"], 0, "{outcome}");
}

fn default_ux_rules_pipeline_file_step_binds_optional_inputs() {
    let (_root, runtime) = fixture("[ux_rules]\nmax_new_tasks = 2\nfile_at = \"high\"\n");
    let activity_dir = _root.path().join("home/.orbit/resources/activities");
    std::fs::create_dir_all(&activity_dir).unwrap();
    std::fs::write(
        activity_dir.join("file_rule_findings.yaml"),
        include_str!("../../assets/activities/file_rule_findings.yaml"),
    )
    .unwrap();

    let mut pipeline: serde_yaml::Value =
        serde_yaml::from_str(include_str!("../../assets/jobs/ux_rules_pipeline.yaml")).unwrap();
    let steps = pipeline["spec"]["steps"].as_sequence().unwrap();
    let mut file_step = steps[1].clone();
    file_step["default_input"]["results"] = serde_yaml::to_value(document(Vec::new())).unwrap();
    pipeline["spec"]["steps"] = serde_yaml::Value::Sequence(vec![file_step]);

    let job_path = _root.path().join("repo/ux_rules_pipeline_file_step.yaml");
    std::fs::write(&job_path, serde_yaml::to_string(&pipeline).unwrap()).unwrap();
    let outcome = runtime.run_job_v2_from_yaml(&job_path, json!({})).unwrap();
    assert!(outcome.success, "{outcome:?}");
}

fn errors_file_one_task_per_rule() {
    let (_root, runtime) = fixture("");
    let mut first_error = result("error", "medium", "cli.example|a", json!({"threw": "x"}));
    let mut second_error = result("error", "medium", "cli.example|b", json!({"threw": "y"}));
    first_error["target"] = json!({"id": "a"});
    second_error["target"] = json!({"id": "b"});
    let outcome = file(
        &runtime,
        document(vec![first_error, second_error]),
        json!({}),
    );
    assert_eq!(outcome["filed_count"], 1, "{outcome}");
    assert_eq!(outcome["filed"][0]["error_task"], true);
    let task_id = outcome["filed"][0]["task_id"].as_str().unwrap();
    let task = runtime.get_task(task_id).unwrap();
    assert!(task.tags.iter().any(|tag| tag == "ux-rules-error"));
    assert_eq!(task.rule_fingerprint.as_deref(), Some("error|cli.example"));
}

fn duplicate_failing_fingerprint_in_one_document_files_once() {
    let (_root, runtime) = fixture("");
    let outcome = file(
        &runtime,
        document(vec![
            result("fail", "low", "fp-dup", json!({"gap": 1})),
            result("fail", "critical", "fp-dup", json!({"gap": 2})),
            result("fail", "high", "fp-other", json!({"gap": 3})),
        ]),
        json!({}),
    );
    assert_eq!(outcome["counts"]["fail"], 3, "{outcome}");
    assert_eq!(outcome["filed_count"], 2, "{outcome}");
    let dup_filed: Vec<_> = outcome["filed"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|entry| entry["fingerprint"] == "fp-dup")
        .collect();
    assert_eq!(dup_filed.len(), 1, "{outcome}");
    // Highest severity wins deterministically for the repeated fingerprint.
    assert_eq!(dup_filed[0]["severity"], "critical", "{outcome}");
    let dup_task_id = dup_filed[0]["task_id"].as_str().unwrap();
    assert_eq!(
        runtime
            .get_task(dup_task_id)
            .unwrap()
            .rule_fingerprint
            .as_deref(),
        Some("fp-dup")
    );
    // The duplicate is reported, not silently double-filed.
    let skipped = outcome["skipped_existing"].as_array().unwrap();
    assert_eq!(skipped.len(), 1, "{outcome}");
    assert_eq!(skipped[0]["fingerprint"], "fp-dup", "{outcome}");
    assert_eq!(skipped[0]["task_id"], dup_task_id, "{outcome}");

    // Identical repeats in one document also file exactly once.
    let (_root2, runtime2) = fixture("");
    let identical = file(
        &runtime2,
        document(vec![
            result("fail", "high", "fp-same", json!({"gap": 1})),
            result("fail", "high", "fp-same", json!({"gap": 1})),
        ]),
        json!({}),
    );
    assert_eq!(identical["filed_count"], 1, "{identical}");
    assert_eq!(
        identical["skipped_existing"].as_array().unwrap().len(),
        1,
        "{identical}"
    );
    assert_eq!(identical["skipped_existing"][0]["fingerprint"], "fp-same");
}

crate::scenario_table!(
    rule_findings_sweep_scenarios,
    duplicate_failing_fingerprint_in_one_document_files_once,
    failing_fingerprint_files_once_then_comments_on_change_and_pass,
    oscillating_measurements_comment_on_return_to_earlier_value,
    fail_pass_fail_pass_records_second_pass_transition,
    cap_takes_highest_severity_first_and_floor_records_only,
    blank_file_at_uses_configured_floor,
    default_ux_rules_pipeline_file_step_binds_optional_inputs,
    errors_file_one_task_per_rule
);
