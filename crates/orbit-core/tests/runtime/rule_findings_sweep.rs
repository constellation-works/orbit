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

fn aggregate_ui_fingerprint_reconciles_fail_to_pass_once() {
    let (_root, runtime) = fixture("");
    // Runner-produced stable aggregate identity (ORB-20885): the selector
    // lives in measurements.failures, never in the fingerprint.
    let fp = "ui.cramped-wrap|analytics-implementer-detail|dark|1440|default";
    let ui = |status: &str, measurements: Value| {
        let mut entry = result(status, "medium", fp, measurements);
        entry["rule"] = json!("ui.cramped-wrap");
        entry["target"] = json!({"id": "analytics-implementer-detail|dark|1440|default", "argv": [], "view": "analytics-implementer-detail", "width": 1440, "theme": "dark", "state": "default"});
        entry
    };
    let fail_dt = document(vec![ui(
        "fail",
        json!({"matched": 3, "failures": [{"selector": "dt", "text": "Cost coverage"}]}),
    )]);
    let first = file(&runtime, fail_dt.clone(), json!({}));
    assert_eq!(first["filed_count"], 1, "{first}");
    let task_id = first["filed"][0]["task_id"].as_str().unwrap().to_string();
    assert_eq!(
        runtime
            .get_task(&task_id)
            .unwrap()
            .rule_fingerprint
            .as_deref(),
        Some(fp)
    );
    // Same failure again: nothing new.
    let second = file(&runtime, fail_dt, json!({}));
    assert_eq!(second["filed_count"], 0, "{second}");
    assert_eq!(second["commented"].as_array().unwrap().len(), 0, "{second}");
    // First failing element changes, same aggregate fingerprint: comment, no new task.
    let fail_other = document(vec![ui(
        "fail",
        json!({"matched": 3, "failures": [{"selector": "span.other", "text": "Other"}]}),
    )]);
    let third = file(&runtime, fail_other, json!({}));
    assert_eq!(third["filed_count"], 0, "{third}");
    assert_eq!(third["commented"].as_array().unwrap().len(), 1, "{third}");
    assert_eq!(third["commented"][0]["kind"], "measurements_changed");
    // Skipped (unmeasured) cannot certify repair.
    let skipped = file(
        &runtime,
        document(vec![ui("skipped", json!({"matched": 0}))]),
        json!({}),
    );
    assert_eq!(
        skipped["commented"].as_array().unwrap().len(),
        0,
        "{skipped}"
    );
    // Error cannot certify repair (it files its own broken-rule task, no pass comment).
    let errored = file(
        &runtime,
        document(vec![ui("error", json!({"message": "boom"}))]),
        json!({}),
    );
    assert_eq!(
        errored["commented"].as_array().unwrap().len(),
        0,
        "{errored}"
    );
    // Readable pass reconciles the same task exactly once, no status change.
    let passing = document(vec![ui("pass", json!({"matched": 3}))]);
    let fourth = file(&runtime, passing.clone(), json!({}));
    assert_eq!(fourth["filed_count"], 0, "{fourth}");
    assert_eq!(fourth["commented"].as_array().unwrap().len(), 1, "{fourth}");
    assert_eq!(fourth["commented"][0]["kind"], "pass");
    assert_eq!(fourth["commented"][0]["task_id"], task_id);
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
    file_step["default_input"]
        .as_mapping_mut()
        .unwrap()
        .remove(serde_yaml::Value::from("evidence_step"));
    file_step["default_input"]
        .as_mapping_mut()
        .unwrap()
        .remove(serde_yaml::Value::from("evidence_files"));
    file_step["default_input"]["results"] = serde_yaml::to_value(document(Vec::new())).unwrap();
    pipeline["spec"]["steps"] = serde_yaml::Value::Sequence(vec![file_step]);

    let job_path = _root.path().join("repo/ux_rules_pipeline_file_step.yaml");
    std::fs::write(&job_path, serde_yaml::to_string(&pipeline).unwrap()).unwrap();
    let outcome = runtime.run_job_v2_from_yaml(&job_path, json!({})).unwrap();
    assert!(outcome.success, "{outcome:?}");
}

fn ux_rules_pipeline_binds_config_defaults_and_explicit_overrides_win() {
    let (_root, runtime) = fixture(
        "[ux_rules]\nsurfaces = [\"cli\"]\nrunner = \"custom/run.mjs\"\nui_base_url = \"http://127.0.0.1:9999\"\n",
    );
    let job_path = _root.path().join("repo/ux_rules_pipeline.yaml");
    std::fs::write(
        &job_path,
        include_str!("../../assets/jobs/ux_rules_pipeline.yaml"),
    )
    .unwrap();
    // A manifest (but no grant) for the script step, and the filer activity,
    // so the job loads and the step refuses at dispatch.
    let scripts_dir = _root.path().join("repo/.orbit/resources/scripts");
    std::fs::create_dir_all(&scripts_dir).unwrap();
    std::fs::create_dir_all(_root.path().join("repo/tools")).unwrap();
    std::fs::write(
        _root.path().join("repo/tools/run.sh"),
        "#!/bin/sh\necho '{}'\n",
    )
    .unwrap();
    std::fs::write(
        scripts_dir.join("ux_rules_runner.yaml"),
        "schemaVersion: 1\nkind: Script\nmetadata:\n  name: ux_rules_runner\nspec:\n  description: Fixture runner.\n  entry: tools/run.sh\n",
    )
    .unwrap();
    let activity_dir = _root.path().join("home/.orbit/resources/activities");
    std::fs::create_dir_all(&activity_dir).unwrap();
    std::fs::write(
        activity_dir.join("file_rule_findings.yaml"),
        include_str!("../../assets/activities/file_rule_findings.yaml"),
    )
    .unwrap();

    // No caller input: effective config supplies surfaces/runner/ui_base_url
    // to the persisted run, which both the script step and (through
    // `input.runner`) the filer consume. The script step itself refuses
    // without a grant, so the run does not succeed or file anything.
    let persisted_input = |runtime: &OrbitRuntime| {
        runtime
            .list_job_runs(orbit_core::application::job::JobRunListParams::default())
            .unwrap()
            .into_iter()
            .filter(|run| run.job_id.as_str() == "ux_rules_pipeline")
            .max_by_key(|run| run.scheduled_at)
            .expect("ux run persisted")
            .input
            .expect("run input persisted")
    };
    let refused = runtime.run_job_v2_from_yaml(&job_path, json!({}));
    assert!(
        format!("{refused:?}").contains("not granted"),
        "{refused:?}"
    );
    let input = persisted_input(&runtime);
    assert_eq!(input["surfaces"], json!(["cli"]), "{input}");
    assert_eq!(input["runner"], "custom/run.mjs", "{input}");
    assert_eq!(input["ui_base_url"], "http://127.0.0.1:9999", "{input}");
    assert!(
        runtime
            .list_tasks_by_tags(&["ux-rules".to_string()])
            .unwrap()
            .is_empty()
    );

    // Explicit overrides win, including an empty surfaces list.
    let _ = runtime.run_job_v2_from_yaml(
        &job_path,
        json!({"surfaces": [], "runner": "explicit.mjs", "ui_base_url": "http://127.0.0.1:1"}),
    );
    let input = persisted_input(&runtime);
    assert_eq!(input["surfaces"], json!([]), "{input}");
    assert_eq!(input["runner"], "explicit.mjs", "{input}");
    assert_eq!(input["ui_base_url"], "http://127.0.0.1:1", "{input}");
}

fn ux_rules_pipeline_preserves_custom_manifest_defaults_when_config_unset() {
    let (_root, runtime) = fixture("");
    let mut pipeline: serde_yaml::Value =
        serde_yaml::from_str(include_str!("../../assets/jobs/ux_rules_pipeline.yaml")).unwrap();
    pipeline["spec"]["default_input"]["surfaces"] =
        serde_yaml::to_value(vec!["cli".to_string()]).unwrap();
    pipeline["spec"]["default_input"]["runner"] = serde_yaml::to_value("custom/run.mjs").unwrap();
    pipeline["spec"]["default_input"]["ui_base_url"] =
        serde_yaml::to_value("http://127.0.0.1:9999").unwrap();
    let job_path = _root.path().join("repo/ux_rules_pipeline_custom.yaml");
    std::fs::write(&job_path, serde_yaml::to_string(&pipeline).unwrap()).unwrap();
    // Same loadable-but-ungranted shape as the configured-defaults scenario,
    // so the only difference under test is the unset-config precedence.
    let scripts_dir = _root.path().join("repo/.orbit/resources/scripts");
    std::fs::create_dir_all(&scripts_dir).unwrap();
    std::fs::create_dir_all(_root.path().join("repo/tools")).unwrap();
    std::fs::write(
        _root.path().join("repo/tools/run.sh"),
        "#!/bin/sh\necho '{}'\n",
    )
    .unwrap();
    std::fs::write(
        scripts_dir.join("ux_rules_runner.yaml"),
        "schemaVersion: 1\nkind: Script\nmetadata:\n  name: ux_rules_runner\nspec:\n  description: Fixture runner.\n  entry: tools/run.sh\n",
    )
    .unwrap();
    let activity_dir = _root.path().join("home/.orbit/resources/activities");
    std::fs::create_dir_all(&activity_dir).unwrap();
    std::fs::write(
        activity_dir.join("file_rule_findings.yaml"),
        include_str!("../../assets/activities/file_rule_findings.yaml"),
    )
    .unwrap();

    let persisted_input = |runtime: &OrbitRuntime| {
        runtime
            .list_job_runs(orbit_core::application::job::JobRunListParams::default())
            .unwrap()
            .into_iter()
            .filter(|run| run.job_id.as_str() == "ux_rules_pipeline")
            .max_by_key(|run| run.scheduled_at)
            .expect("ux run persisted")
            .input
            .expect("run input persisted")
    };
    // Empty-object caller input: unset config must not inject built-ins, so
    // the custom manifest defaults survive engine merging.
    let refused = runtime.run_job_v2_from_yaml(&job_path, json!({}));
    assert!(
        format!("{refused:?}").contains("not granted"),
        "{refused:?}"
    );
    let input = persisted_input(&runtime);
    assert!(
        input.get("surfaces").is_none_or(Value::is_null),
        "unset config must leave surfaces to the manifest default, got {input}"
    );
    assert!(
        input.get("runner").is_none_or(Value::is_null),
        "unset config must leave runner to the manifest default, got {input}"
    );
    assert!(
        input.get("ui_base_url").is_none_or(Value::is_null),
        "unset config must leave ui_base_url to the manifest default, got {input}"
    );

    // Null caller input normalizes the same way.
    let refused = runtime.run_job_v2_from_yaml(&job_path, Value::Null);
    assert!(
        format!("{refused:?}").contains("not granted"),
        "{refused:?}"
    );
    let input = persisted_input(&runtime);
    assert!(
        input.get("surfaces").is_none_or(Value::is_null),
        "unset config must leave surfaces to the manifest default, got {input}"
    );
    assert!(
        input.get("runner").is_none_or(Value::is_null),
        "unset config must leave runner to the manifest default, got {input}"
    );
    assert!(
        input.get("ui_base_url").is_none_or(Value::is_null),
        "unset config must leave ui_base_url to the manifest default, got {input}"
    );

    // Per-key nulls are unsupplied too: remove them from the run input so
    // engine merging can use the custom manifest defaults.
    let refused = runtime.run_job_v2_from_yaml(
        &job_path,
        json!({"surfaces": null, "runner": null, "ui_base_url": null}),
    );
    assert!(
        format!("{refused:?}").contains("not granted"),
        "{refused:?}"
    );
    let input = persisted_input(&runtime);
    assert!(
        input.get("surfaces").is_none(),
        "unset config must remove null surfaces so the manifest default applies, got {input}"
    );
    assert!(
        input.get("runner").is_none(),
        "unset config must remove null runner so the manifest default applies, got {input}"
    );
    assert!(
        input.get("ui_base_url").is_none(),
        "unset config must remove null ui_base_url so the manifest default applies, got {input}"
    );
}

/// The shipped `ux_rules_runner` template (README) must reach the runner
/// through the granted script-step path: granting it records no `env_pass`,
/// and a pipeline run is never refused for `env_pass`. On a host where the
/// sandbox itself cannot start (for example nested in another sandbox) the
/// run may refuse for that reason instead, but not for input validation.
fn documented_ux_rules_runner_template_is_not_refused_for_env_pass() {
    let (_root, runtime) = fixture("");
    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let readme = std::fs::read_to_string(repo_root.join("scripts/ux-rules/README.md")).unwrap();
    let template_start = readme
        .find("name: ux_rules_runner")
        .and_then(|index| readme[..index].rfind("```yaml\n"))
        .unwrap()
        + "```yaml\n".len();
    let template_end = template_start + readme[template_start..].find("\n```").unwrap();
    let template = &readme[template_start..template_end];

    // Adapt the template's host paths exactly as its setup text instructs.
    let node = std::env::var_os("PATH")
        .and_then(|paths| {
            std::env::split_paths(&paths).find_map(|dir| {
                let candidate = dir.join("node");
                candidate
                    .is_file()
                    .then(|| candidate.canonicalize().unwrap())
            })
        })
        .expect("node on PATH for the template interpreter");
    let fake_orbit = _root.path().join("host-bin/orbit");
    std::fs::create_dir_all(fake_orbit.parent().unwrap()).unwrap();
    std::fs::write(&fake_orbit, "#!/bin/sh\nexit 0\n").unwrap();
    let kit_root = _root.path().join("browser-kit");
    std::fs::create_dir_all(kit_root.join("kit/node_modules/playwright")).unwrap();
    std::fs::write(
        kit_root.join("kit/node_modules/playwright/index.mjs"),
        "export {};\n",
    )
    .unwrap();
    std::fs::create_dir_all(kit_root.join("browsers")).unwrap();
    let manifest = template
        .replace("/opt/node/bin/node", &node.display().to_string())
        .replace("/opt/orbit/bin/orbit", &fake_orbit.display().to_string())
        .replace("/opt/browser-kit", &kit_root.display().to_string());

    let repo = _root.path().join("repo");
    let scripts_dir = repo.join(".orbit/resources/scripts");
    std::fs::create_dir_all(&scripts_dir).unwrap();
    std::fs::write(scripts_dir.join("ux_rules_runner.yaml"), &manifest).unwrap();
    let entry_path = repo.join("scripts/ux-rules/step.mjs");
    std::fs::create_dir_all(entry_path.parent().unwrap()).unwrap();
    std::fs::copy(repo_root.join("scripts/ux-rules/step.mjs"), &entry_path).unwrap();
    let activity_dir = _root.path().join("home/.orbit/resources/activities");
    std::fs::create_dir_all(&activity_dir).unwrap();
    std::fs::write(
        activity_dir.join("file_rule_findings.yaml"),
        include_str!("../../assets/activities/file_rule_findings.yaml"),
    )
    .unwrap();

    let grant = runtime
        .grant_script(
            "ux_rules_runner",
            &orbit_core::runtime::scripts::ScriptGrantOptions {
                output_max_files: Some(64),
                output_max_file_bytes: Some(1048576),
                ..Default::default()
            },
        )
        .expect("the documented template grants");
    assert!(
        grant.env_pass.is_empty(),
        "the documented template must not record env_pass: {:?}",
        grant.env_pass
    );

    let job_path = repo.join("ux_rules_pipeline.yaml");
    std::fs::write(
        &job_path,
        include_str!("../../assets/jobs/ux_rules_pipeline.yaml"),
    )
    .unwrap();
    let outcome = runtime.run_job_v2_from_yaml(&job_path, json!({}));
    let rendered = format!("{outcome:?}");
    assert!(
        !rendered.contains("env_pass"),
        "the granted template step must not be refused for env_pass: {rendered}"
    );
    assert!(
        !rendered.contains("script_input_invalid") && !rendered.contains("input_schema_json"),
        "the default step input must satisfy the documented schema: {rendered}"
    );
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

fn mixed_surface_results_keep_their_filing_surface() {
    let (_root, runtime) = fixture("");
    let mut cli = result("fail", "high", "mixed-cli", json!({}));
    cli["surface"] = json!("cli");
    let mut ui = result("error", "high", "mixed-ui", json!({}));
    ui["surface"] = json!("ui");
    ui["rule"] = json!("ui.example");
    let outcome = file(
        &runtime,
        json!({"schema_version": 1, "surface": "mixed", "results": [cli, ui]}),
        json!({}),
    );
    assert_eq!(outcome["filed_count"], 2, "{outcome}");
    for entry in outcome["filed"].as_array().unwrap() {
        let task = runtime
            .get_task(entry["task_id"].as_str().unwrap())
            .unwrap();
        let surface = if entry["error_task"] == true {
            "ui"
        } else {
            "cli"
        };
        assert!(
            task.tags.iter().any(|tag| tag == surface),
            "{:?}",
            task.tags
        );
        assert!(
            task.description.contains(&format!("--surface {surface}")),
            "{}",
            task.description
        );
    }
}

crate::scenario_table!(
    rule_findings_sweep_scenarios,
    mixed_surface_results_keep_their_filing_surface,
    duplicate_failing_fingerprint_in_one_document_files_once,
    failing_fingerprint_files_once_then_comments_on_change_and_pass,
    aggregate_ui_fingerprint_reconciles_fail_to_pass_once,
    oscillating_measurements_comment_on_return_to_earlier_value,
    fail_pass_fail_pass_records_second_pass_transition,
    cap_takes_highest_severity_first_and_floor_records_only,
    blank_file_at_uses_configured_floor,
    documented_ux_rules_runner_template_is_not_refused_for_env_pass,
    default_ux_rules_pipeline_file_step_binds_optional_inputs,
    ux_rules_pipeline_binds_config_defaults_and_explicit_overrides_win,
    ux_rules_pipeline_preserves_custom_manifest_defaults_when_config_unset,
    errors_file_one_task_per_rule
);
