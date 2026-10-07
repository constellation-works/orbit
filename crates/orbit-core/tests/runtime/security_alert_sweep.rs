//! Security sweeps use config layering and report what the floor excluded.
//! Regression for the moderate alerts silently omitted on 2026-10-06/07.

use orbit_core::OrbitRuntime;
use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use serde_json::{Value, json};

use super::dispatch_admission::isolated;

fn fixture(global: Option<&str>, workspace: Option<&str>) -> (tempfile::TempDir, OrbitRuntime) {
    let root = tempfile::tempdir().unwrap();
    let global_root = root.path().join("home/.orbit");
    let workspace_root = root.path().join("repo/.orbit");
    for (path, severity) in [(&global_root, global), (&workspace_root, workspace)] {
        std::fs::create_dir_all(path).unwrap();
        // An existing workspace file with no floor must still inherit global.
        let config = severity.map_or_else(String::new, |severity| {
            format!("[security_alert_sweep]\nmin_severity = {severity:?}\n")
        });
        std::fs::write(path.join("config.toml"), config).unwrap();
    }
    let runtime = OrbitRuntime::from_roots(&global_root, &workspace_root).unwrap();
    (root, runtime)
}

fn snapshot() -> Value {
    json!({
        "schema_version": 2, "collected": true,
        "collection_status": "fully_collected",
        "repository": {"full_name": "acme/orbit"},
        "open_alerts": [
            {"number": 71, "severity": "moderate", "ecosystem": "cargo", "package": "rustls", "manifest_path": "Cargo.lock"},
            {"number": 72, "severity": "low", "ecosystem": "npm", "package": "low-package", "manifest_path": "website/package-lock.json"}
        ],
        "open_dependabot_pull_requests": []
    })
}

fn file(runtime: &OrbitRuntime, snapshot: Value, severity: Option<&str>) -> Value {
    let mut input = json!({"dependabot_snapshot": snapshot});
    if let Some(severity) = severity {
        input["min_severity"] = json!(severity);
    }
    runtime
        .run_deterministic(
            "file_dependabot_alert_tasks",
            &json!({}),
            &input,
            ToolContext::default(),
        )
        .unwrap()
}

#[test]
fn filing_applies_each_config_layer_and_reports_the_effective_source() {
    if !isolated(
        "security_alert_sweep::filing_applies_each_config_layer_and_reports_the_effective_source",
    ) {
        return;
    }
    for (global, workspace, floor, source, filed, excluded) in [
        (None, None, "moderate", "built-in", 1, vec![72]),
        (None, Some("high"), "high", "workspace", 0, vec![71, 72]),
        (Some("high"), None, "high", "global", 0, vec![71, 72]),
        (
            Some("critical"),
            Some("moderate"),
            "moderate",
            "workspace",
            1,
            vec![72],
        ),
        (
            Some("moderate"),
            Some("high"),
            "high",
            "workspace",
            0,
            vec![71, 72],
        ),
    ] {
        let (_root, runtime) = fixture(global, workspace);
        let output = file(&runtime, snapshot(), None);
        assert_eq!(output["min_severity"], floor);
        assert_eq!(output["min_severity_source"], source);
        assert_eq!(
            output["filed_count"], filed,
            "moderate findings must not silently disappear (2026-10-06/07 sweep incident)"
        );
        let numbers: Vec<_> = output["excluded_below_min_severity"]
            .as_array()
            .unwrap()
            .iter()
            .map(|alert| alert["number"].as_u64().unwrap())
            .collect();
        assert_eq!(numbers, excluded);
        if filed > 0 {
            let task = runtime
                .get_task(output["filed"][0]["task_id"].as_str().unwrap())
                .unwrap();
            assert!(task.tags.iter().any(|tag| tag == "dependabot-sweep"));
            assert!(task.description.contains("rustls"));
        }
    }
}

#[test]
fn input_overrides_config_for_one_filing_and_secrets_ignore_the_floor() {
    if !isolated(
        "security_alert_sweep::input_overrides_config_for_one_filing_and_secrets_ignore_the_floor",
    ) {
        return;
    }
    let (_root, runtime) = fixture(Some("high"), Some("moderate"));
    let mut snapshot = snapshot();
    snapshot["code_scanning"] = json!({"collected": true, "open_alerts": [{
        "number": 81, "security_severity": "moderate", "rule_id": "rust/example",
        "path": "src/lib.rs", "message": "Unsafe input", "tool_name": "CodeQL"
    }]});
    snapshot["secret_scanning"] = json!({"collected": true, "open_alerts": [{
        "number": 91, "secret_type": "fixture_token", "secret_type_display_name": "Fixture token", "locations": []
    }]});
    let override_output = file(&runtime, snapshot.clone(), Some("critical"));
    assert_eq!(override_output["min_severity"], "critical");
    assert_eq!(override_output["min_severity_source"], "input");
    assert_eq!(override_output["filed_count"], 1);
    assert_eq!(override_output["filed"][0]["family"], "secret_scanning");
    assert_eq!(
        override_output["excluded_below_min_severity"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        override_output["excluded_below_min_severity"][2]["alert_number"],
        81
    );

    let configured_output = file(&runtime, snapshot, None);
    assert_eq!(configured_output["min_severity"], "moderate");
    assert_eq!(configured_output["min_severity_source"], "workspace");
    assert_eq!(configured_output["filed_count"], 2);
    assert_eq!(
        configured_output["excluded_below_min_severity"][0]["number"],
        72
    );
}

#[test]
fn invalid_severity_is_rejected_by_config_admission_with_its_key() {
    if !isolated(
        "security_alert_sweep::invalid_severity_is_rejected_by_config_admission_with_its_key",
    ) {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let global = root.path().join("global");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&global).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    for invalid_layer in [&global, &workspace] {
        std::fs::write(global.join("config.toml"), "").unwrap();
        std::fs::write(workspace.join("config.toml"), "").unwrap();
        std::fs::write(
            invalid_layer.join("config.toml"),
            "[security_alert_sweep]\nmin_severity = \"urgent\"\n",
        )
        .unwrap();
        let error = orbit_config::load_effective_config(&orbit_config::ConfigRoots::new(
            &global, &workspace,
        ))
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("security_alert_sweep.min_severity"),
            "{error}"
        );
    }
}
