//! Security sweeps use config layering, report what the floor excluded, and
//! consolidate historical per-alert tasks without losing duplicate owners.
//! Regression for the moderate alerts silently omitted on 2026-10-06/07.

use orbit_core::application::task::TaskAddParams;
use orbit_core::{OrbitRuntime, TaskComplexity, TaskStatus};
use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use orbit_types::task::TaskRelationType;
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
            {"number": 71, "severity": "moderate", "ecosystem": "rust", "package": "rustls", "manifest_path": "Cargo.lock"},
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
fn consolidation_retires_every_duplicate_alert_owner_once() {
    if !isolated("security_alert_sweep::consolidation_retires_every_duplicate_alert_owner_once") {
        return;
    }
    // Duplicate owners alone also need consolidation even though the group
    // contains only one unique alert.
    for numbers in [vec![81, 81], vec![81, 81, 82]] {
        let (root, runtime) = fixture(None, None);
        let prerequisite = runtime
            .add_task(TaskAddParams {
                title: "Shared prerequisite".to_string(),
                complexity: TaskComplexity::Medium,
                status: Some(TaskStatus::Backlog),
                ..Default::default()
            })
            .unwrap();
        let mut source_ids = Vec::new();
        let mut selectors = Vec::new();
        for (index, number) in numbers.iter().enumerate() {
            let path = format!("source-{index}.rs");
            std::fs::write(root.path().join("repo").join(&path), "").unwrap();
            let selector = format!("file:{path}");
            let source = runtime
                .add_task(TaskAddParams {
                    title: format!("Historical alert {number}, owner {index}"),
                    description: format!(
                        "## Alert evidence\n\n\
                         - Repository: `acme/orbit`\n\
                         - Alert: `#{number}`\n\
                         - Rule: `rust/example` (Unsafe input)\n\
                         - Security severity: `high`\n\
                         - Tool: `CodeQL` (version `1`, guid `rust`)\n\
                         - Message: Unsafe input\n\
                         - Ref: `refs/heads/main`\n\
                         - Commit: `fixture`\n\
                         - Location: `src/lib.rs` line {number}\n"
                    ),
                    tags: vec![
                        "code-scanning-sweep".to_string(),
                        format!("code-scanning:fixture-{number}"),
                    ],
                    // Only the earlier duplicate carries this external
                    // dependency; dependencies on replaced owners disappear.
                    dependencies: vec![
                        source_ids
                            .first()
                            .cloned()
                            .unwrap_or_else(|| prerequisite.id.clone()),
                    ],
                    context_files: vec![selector.clone()],
                    complexity: TaskComplexity::Medium,
                    status: Some(TaskStatus::Backlog),
                    ..Default::default()
                })
                .unwrap();
            source_ids.push(source.id);
            selectors.push(selector);
        }
        source_ids.sort();
        let mut unique_numbers = numbers.clone();
        unique_numbers.sort_unstable();
        unique_numbers.dedup();
        let consolidate = |apply| {
            runtime
                .run_deterministic(
                    "consolidate_code_scanning_tasks",
                    &json!({}),
                    &json!({"apply": apply}),
                    ToolContext::default(),
                )
                .unwrap()
        };

        let preview = consolidate(false);
        assert_eq!(preview["outcome"], "dry_run");
        assert_eq!(preview["scanned_source_tasks"], numbers.len());
        assert_eq!(preview["group_count"], 1);
        assert_eq!(preview["unchanged_single_source"], json!([]));
        assert_eq!(preview["skipped"], json!([]));
        assert_eq!(preview["groups"][0]["source_task_ids"], json!(source_ids));
        assert_eq!(preview["groups"][0]["alert_numbers"], json!(unique_numbers));
        for id in &source_ids {
            assert_eq!(runtime.get_task(id).unwrap().status, TaskStatus::Backlog);
        }

        let applied = consolidate(true);
        assert_eq!(applied["outcome"], "applied");
        assert_eq!(applied["group_count"], 1);
        let group = &applied["groups"][0];
        assert_eq!(group["source_task_ids"], json!(source_ids));
        assert_eq!(group["alert_numbers"], json!(unique_numbers));
        assert_eq!(group["applied"], true);
        assert_eq!(group["sources_not_retired"], json!([]));
        let replacement = runtime
            .get_task(group["replacement_task_id"].as_str().unwrap())
            .unwrap();
        assert_eq!(replacement.status, TaskStatus::Backlog);
        assert_eq!(replacement.dependencies(), vec![prerequisite.id]);
        assert_eq!(replacement.context_files, selectors);
        let superseded: Vec<_> = replacement
            .relations
            .iter()
            .filter(|relation| relation.relation_type == TaskRelationType::Supersedes)
            .map(|relation| relation.target.clone())
            .collect();
        assert_eq!(superseded, source_ids);
        assert_eq!(
            replacement
                .tags
                .iter()
                .filter(|tag| tag.starts_with("code-scanning:"))
                .count(),
            unique_numbers.len(),
            "replacement coverage must contain one key per unique alert"
        );
        for id in &source_ids {
            assert_eq!(runtime.get_task(id).unwrap().status, TaskStatus::Rejected);
        }
        let repeated = consolidate(true);
        assert_eq!(repeated["outcome"], "nothing_to_consolidate");
        assert_eq!(repeated["groups"], json!([]));
    }
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
