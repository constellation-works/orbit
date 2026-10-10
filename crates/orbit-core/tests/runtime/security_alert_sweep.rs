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

fn code_alert(number: u64, rule: &str, path: &str, line: u64) -> Value {
    json!({
        "number": number, "rule_id": rule, "rule_name": "Cleartext logging",
        "security_severity": "high", "path": path, "start_line": line,
        "end_line": line, "message": "Logs sensitive data",
        "tool_name": "CodeQL", "tool_version": "2", "tool_guid": "codeql",
        "ref": "refs/heads/main", "commit_sha": "fixture",
    })
}

fn code_alerts_snapshot(alerts: Vec<Value>) -> Value {
    json!({
        "schema_version": 2, "collected": false,
        "repository": {"full_name": "acme/orbit"},
        "open_alerts": [],
        "open_dependabot_pull_requests": [],
        "code_scanning": {
            "collected": true,
            "collection_status": "fully_collected",
            "outcome_hint": "open_alerts",
            "open_alerts": alerts,
        },
    })
}

fn code_snapshot(number: u64, rule: &str, path: &str) -> Value {
    code_alerts_snapshot(vec![code_alert(number, rule, path, 12)])
}

fn assert_rejected_owner(report: &Value, number: u64, owner: &str) {
    let matched: Vec<_> = report["skipped_existing"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["alert_number"] == number)
        .collect();
    assert_eq!(matched.len(), 1, "{report}");
    assert_eq!(matched[0]["family"], "code_scanning");
    assert_eq!(matched[0]["match_kind"], "rejected_owner");
    assert_eq!(matched[0]["task_id"], owner);
    assert!(
        matched[0]["match_evidence"]["matched_fields"]
            .as_array()
            .unwrap()
            .iter()
            .any(|field| field["field"] == "rejected_task_id" && field["value"] == owner),
        "the audit must name the rejected task: {matched:?}"
    );
}

fn code_tasks(runtime: &OrbitRuntime) -> usize {
    runtime
        .list_tasks()
        .unwrap()
        .iter()
        .filter(|task| task.tags.iter().any(|tag| tag == "code-scanning-sweep"))
        .count()
}

/// A rejected implementer verdict on an unchanged alert must stick: the sweep
/// re-filed alert #484 thirteen minutes after its owner was rejected as a false
/// positive (on-call ORB-15178, failure class B10).
#[test]
fn rejected_code_scanning_owner_suppresses_refiling_until_the_alert_changes() {
    if !isolated(
        "security_alert_sweep::rejected_code_scanning_owner_suppresses_refiling_until_the_alert_changes",
    ) {
        return;
    }
    let (_root, runtime) = fixture(None, None);
    let rule = "rust/cleartext-logging";
    let path = "crates/a/src/lib.rs";

    let first = file(&runtime, code_snapshot(484, rule, path), None);
    assert_eq!(first["filed_count"], 1);
    let owner = first["filed"][0]["task_id"].as_str().unwrap().to_string();

    // While the owner is open it covers the alert, as before.
    let covered = file(&runtime, code_snapshot(484, rule, path), None);
    assert_eq!(covered["filed_count"], 0);
    assert_eq!(covered["skipped_existing"][0]["match_kind"], "exact_key");

    runtime
        .reject_task(
            &owner,
            "false positive: trusted_* name heuristic".to_string(),
            None,
        )
        .unwrap();

    let suppressed = file(&runtime, code_snapshot(484, rule, path), None);
    assert_eq!(suppressed["filed_count"], 0, "{suppressed}");
    assert_eq!(suppressed["clusters"], 1);
    let skipped = suppressed["skipped_existing"].as_array().unwrap();
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0]["family"], "code_scanning");
    assert_eq!(skipped[0]["alert_number"], 484);
    assert_eq!(skipped[0]["match_kind"], "rejected_owner");
    assert_eq!(skipped[0]["task_id"], owner.as_str());
    assert!(
        skipped[0]["match_evidence"]["matched_fields"]
            .as_array()
            .unwrap()
            .iter()
            .any(|field| field["field"] == "rejected_task_id" && field["value"] == owner.as_str()),
        "the audit must name the rejected task: {skipped:?}"
    );
    assert_eq!(code_tasks(&runtime), 1);

    // A changed location or rule is a different finding and files again.
    for changed in [
        code_snapshot(484, rule, "crates/b/src/lib.rs"),
        code_snapshot(484, "rust/path-injection", path),
    ] {
        let (_root, runtime) = fixture(None, None);
        let first = file(&runtime, code_snapshot(484, rule, path), None);
        let owner = first["filed"][0]["task_id"].as_str().unwrap().to_string();
        runtime
            .reject_task(&owner, "false positive".to_string(), None)
            .unwrap();
        let refiled = file(&runtime, changed, None);
        assert_eq!(refiled["filed_count"], 1, "{refiled}");
        assert_eq!(refiled["skipped_existing"], json!([]));
    }
}

/// Rejection binds the alert's exact rule and path. A path or rule that differs
/// only by punctuation or case is a different finding and files again, while a
/// longer rule or line number sharing the recorded prefix does not inherit the
/// suppression either.
#[test]
fn rejected_owner_distinguishes_punctuation_only_path_and_rule_changes() {
    if !isolated(
        "security_alert_sweep::rejected_owner_distinguishes_punctuation_only_path_and_rule_changes",
    ) {
        return;
    }
    let rule = "rust/cleartext-logging";
    let path = "crates/foo-bar/src/lib.rs";

    let (_root, runtime) = fixture(None, None);
    let first = file(&runtime, code_snapshot(484, rule, path), None);
    assert_eq!(first["filed_count"], 1, "{first}");
    let owner = first["filed"][0]["task_id"].as_str().unwrap().to_string();
    runtime
        .reject_task(&owner, "false positive".to_string(), None)
        .unwrap();

    let unchanged = file(&runtime, code_snapshot(484, rule, path), None);
    assert_eq!(unchanged["filed_count"], 0, "{unchanged}");
    assert_rejected_owner(&unchanged, 484, &owner);

    for changed in [
        code_snapshot(484, rule, "crates/foo_bar/src/lib.rs"),
        code_snapshot(484, "rust/cleartext_logging", path),
        code_snapshot(484, "Rust/Cleartext-Logging", path),
        code_snapshot(484, "rust/cleartext-logging-v2", path),
        code_alerts_snapshot(vec![code_alert(484, rule, path, 120)]),
    ] {
        let (_root, runtime) = fixture(None, None);
        let first = file(&runtime, code_snapshot(484, rule, path), None);
        let owner = first["filed"][0]["task_id"].as_str().unwrap().to_string();
        runtime
            .reject_task(&owner, "false positive".to_string(), None)
            .unwrap();
        let refiled = file(&runtime, changed, None);
        assert_eq!(refiled["filed_count"], 1, "{refiled}");
        assert_eq!(refiled["skipped_existing"], json!([]));
    }
}

/// A rejected group records one ledger bullet per alert. Moving one alert onto
/// a sibling's old location must file that alert again; the unchanged sibling
/// stays suppressed by the same rejected task.
#[test]
fn rejected_grouped_owner_refills_alert_moved_onto_a_sibling_location() {
    if !isolated(
        "security_alert_sweep::rejected_grouped_owner_refills_alert_moved_onto_a_sibling_location",
    ) {
        return;
    }
    let (_root, runtime) = fixture(None, None);
    let rule = "rust/cleartext-logging";
    let original = code_alerts_snapshot(vec![
        code_alert(484, rule, "crates/a/src/lib.rs", 12),
        code_alert(485, rule, "crates/b/src/lib.rs", 12),
    ]);

    let first = file(&runtime, original.clone(), None);
    assert_eq!(first["filed_count"], 1, "{first}");
    assert_eq!(first["filed"][0]["alert_numbers"], json!([484, 485]));
    assert_eq!(first["filed"][0]["alert_count"], 2);
    let owner = first["filed"][0]["task_id"].as_str().unwrap().to_string();

    let covered = file(&runtime, original.clone(), None);
    assert_eq!(covered["filed_count"], 0, "{covered}");
    for number in [484, 485] {
        let matched: Vec<_> = covered["skipped_existing"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["alert_number"] == number)
            .collect();
        assert_eq!(matched.len(), 1, "{covered}");
        assert_eq!(matched[0]["match_kind"], "exact_key");
        assert_eq!(matched[0]["task_id"], owner.as_str());
    }

    runtime
        .reject_task(&owner, "false positive".to_string(), None)
        .unwrap();

    let suppressed = file(&runtime, original, None);
    assert_eq!(suppressed["filed_count"], 0, "{suppressed}");
    assert_eq!(suppressed["skipped_existing"].as_array().unwrap().len(), 2);
    assert_rejected_owner(&suppressed, 484, &owner);
    assert_rejected_owner(&suppressed, 485, &owner);
    assert_eq!(code_tasks(&runtime), 1);

    // Alert 484 now sits at alert 485's recorded location. The sibling's
    // bullet must not certify 484.
    let moved = code_alerts_snapshot(vec![
        code_alert(484, rule, "crates/b/src/lib.rs", 12),
        code_alert(485, rule, "crates/b/src/lib.rs", 12),
    ]);
    let refiled = file(&runtime, moved, None);
    assert_eq!(refiled["filed_count"], 1, "{refiled}");
    assert_eq!(refiled["filed"][0]["alert_numbers"], json!([484]));
    assert_eq!(refiled["filed"][0]["alert_count"], 1);
    assert_ne!(refiled["filed"][0]["task_id"], owner.as_str());
    assert_eq!(refiled["skipped_existing"].as_array().unwrap().len(), 1);
    assert_rejected_owner(&refiled, 485, &owner);
    assert_eq!(code_tasks(&runtime), 2);
}

/// Consolidation and duplicate rejections point at another owner. They are not
/// a verdict on the alert, so a still-open alert whose covering task finished
/// must be filed again.
#[test]
fn rejection_naming_a_covering_task_does_not_suppress_code_scanning_refiling() {
    if !isolated(
        "security_alert_sweep::rejection_naming_a_covering_task_does_not_suppress_code_scanning_refiling",
    ) {
        return;
    }
    let (_root, runtime) = fixture(None, None);
    let snapshot = || code_snapshot(484, "rust/cleartext-logging", "crates/a/src/lib.rs");
    let first = file(&runtime, snapshot(), None);
    let owner = first["filed"][0]["task_id"].as_str().unwrap().to_string();
    runtime
        .reject_task(
            &owner,
            "Consolidated".to_string(),
            Some("Consolidated into covering task ORB-1: one task owns the repair.".to_string()),
        )
        .unwrap();

    let refiled = file(&runtime, snapshot(), None);
    assert_eq!(refiled["filed_count"], 1, "{refiled}");
}
