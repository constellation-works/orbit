//! Security sweeps apply the fixed moderate floor, report what it excluded, and
//! consolidate historical per-alert tasks without losing duplicate owners.
//! Regression for the moderate alerts silently omitted on 2026-10-06/07.
//! Code scanning repairs whose rule the owner's installed CodeQL bundle lacks
//! are held behind one bundle prerequisite (ORB-26147, ORB-26162).
// The installed-CodeQL fixture is a POSIX shell script.
#![cfg(unix)]

use std::path::Path;

use orbit_core::adapter::command::ToolEntryPoint;
use orbit_core::application::task::{EXTERNAL_MERGE_SETTLED_EVENT, TaskAddParams};
use orbit_core::runtime::OrbitRuntime;
use orbit_engine::{Dispatch, RunLifecycle};
use orbit_tools::ToolContext;
use orbit_types::task::{CodeScanningAlertEvidence, TaskDecision, TaskRelation, TaskRelationType};
use orbit_types::task::{TaskComplexity, TaskHistoryEntry, TaskStatus};
use orbit_types::tool::{McpCapability, ToolSessionContext};
use orbit_types::workflow::{JobRunState, PipelineState};
use serde_json::{Value, json};

struct CodeScanningHistory {
    pre_fix_sha: String,
    landed_shas: [String; 3],
    post_fix_sha: String,
    diverged_sha: String,
}

fn fixture() -> (tempfile::TempDir, OrbitRuntime) {
    let root = tempfile::tempdir_in(orbit_common::test_env::bulk_write_temp_dir()).unwrap();
    let global_root = root.path().join("home/.orbit");
    let workspace_root = root.path().join("repo/.orbit");
    for path in [&global_root, &workspace_root] {
        std::fs::create_dir_all(path).unwrap();
        std::fs::write(path.join("config.toml"), "").unwrap();
    }
    // Every rule these fixtures report is offered by the installed bundle,
    // so filing does not depend on the host's own CodeQL installation.
    install_codeql_bundle(
        root.path(),
        "2.27.1",
        &[
            "rust/cleartext-logging",
            "rust/path-injection",
            "rust/example",
        ],
    );
    let runtime = OrbitRuntime::from_roots(&global_root, &workspace_root).unwrap();
    (root, runtime)
}

/// A `codeql` CLI reporting `version` and resolving one query per rule,
/// placed ahead of the owner's resolved validation PATH.
fn install_codeql_bundle(root: &Path, version: &str, rules: &[&str]) {
    use std::os::unix::fs::PermissionsExt as _;
    let bin = root.join("codeql-bin");
    let queries = root.join("codeql-queries");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::create_dir_all(&queries).unwrap();
    let paths = rules
        .iter()
        .enumerate()
        .map(|(index, rule)| {
            let path = queries.join(format!("Query{index}.ql"));
            std::fs::write(
                &path,
                format!("/**\n * @name Fixture\n * @kind problem\n * @id {rule}\n */\nselect 1\n"),
            )
            .unwrap();
            path.display().to_string()
        })
        .collect::<Vec<_>>();
    let script = format!(
        "#!/bin/sh\ncase \"$1\" in\n  version) printf '%s\\n' '{}' ;;\n  resolve) printf '%s\\n' '{}' ;;\n  *) exit 2 ;;\nesac\n",
        json!({"version": version}),
        json!(paths),
    );
    let cli = bin.join("codeql");
    std::fs::write(&cli, script).unwrap();
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(
        root.join("repo/.orbit/config.toml"),
        format!(
            "[workflow.validation_env]\npath = [{:?}]\n",
            bin.display().to_string()
        ),
    )
    .unwrap();
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
    orbit_core::test_support::EngineHost::new(runtime)
        .run_deterministic(
            "file_dependabot_alert_tasks",
            &json!({}),
            &input,
            ToolContext::default(),
        )
        .unwrap()
}

fn consolidation_retires_every_duplicate_alert_owner_once() {
    // Duplicate owners alone also need consolidation even though the group
    // contains only one unique alert.
    for numbers in [vec![81, 81], vec![81, 81, 82]] {
        let (root, runtime) = fixture();
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
            orbit_core::test_support::EngineHost::new(&runtime)
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

fn filing_applies_the_fixed_floor_and_reports_its_source() {
    let (_root, runtime) = fixture();
    let output = file(&runtime, snapshot(), None);
    assert_eq!(output["min_severity"], "moderate");
    assert_eq!(output["min_severity_source"], "built-in");
    assert_eq!(
        output["filed_count"], 1,
        "moderate findings must not silently disappear (2026-10-06/07 sweep incident)"
    );
    let numbers: Vec<_> = output["excluded_below_min_severity"]
        .as_array()
        .unwrap()
        .iter()
        .map(|alert| alert["number"].as_u64().unwrap())
        .collect();
    assert_eq!(numbers, vec![72]);
    let task = runtime
        .get_task(output["filed"][0]["task_id"].as_str().unwrap())
        .unwrap();
    assert!(task.tags.iter().any(|tag| tag == "dependabot-sweep"));
    assert!(task.description.contains("rustls"));
}

fn input_overrides_the_floor_for_one_filing_and_secrets_ignore_it() {
    let (_root, runtime) = fixture();
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
    assert_eq!(configured_output["min_severity_source"], "built-in");
    assert_eq!(configured_output["filed_count"], 2);
    assert_eq!(
        configured_output["excluded_below_min_severity"][0]["number"],
        72
    );
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

fn write_repo_file(root: &tempfile::TempDir, path: &str) {
    let full = root.path().join("repo").join(path);
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(full, "fixture\n").unwrap();
}

fn dependabot_alert(number: u64, package: &str, manifest_path: &str) -> Value {
    json!({
        "number": number, "severity": "high", "ecosystem": "cargo", "package": package,
        "manifest_path": manifest_path, "scope": "runtime",
        "ghsa_id": "GHSA-fixture", "cve_id": "", "summary": "fixture advisory",
        "vulnerable_version_range": "< 1.0.1", "first_patched_version": "1.0.1",
        "created_at": "fixture", "updated_at": "fixture",
        "html_url": "https://example.invalid/advisory",
    })
}

fn dependabot_snapshot_for(alerts: Vec<Value>) -> Value {
    json!({
        "schema_version": 2, "collected": true,
        "collection_status": "fully_collected",
        "repository": {"full_name": "acme/orbit"},
        "open_alerts": alerts,
        "open_dependabot_pull_requests": [],
    })
}

fn sweep_tasks_seed_file_selectors_from_alert_locations() {
    let (root, runtime) = fixture();
    for path in [
        "crates/a/src/lib.rs",
        "crates/b/src/lib.rs",
        "Cargo.toml",
        "Cargo.lock",
    ] {
        write_repo_file(&root, path);
    }
    let code = code_alerts_snapshot(vec![
        code_alert(484, "rust/cleartext-logging", "crates/a/src/lib.rs", 12),
        code_alert(485, "rust/cleartext-logging", "crates/b/src/lib.rs", 12),
    ]);
    let filed = file(&runtime, code, None);
    assert_eq!(filed["filed_count"], 1, "{filed}");
    let task = runtime
        .get_task(filed["filed"][0]["task_id"].as_str().unwrap())
        .unwrap();
    assert_eq!(
        task.context_files,
        vec![
            "file:crates/a/src/lib.rs".to_string(),
            "file:crates/b/src/lib.rs".to_string()
        ],
        "a code-scanning group carries one selector per distinct alert path"
    );

    let (root, runtime) = fixture();
    for path in ["Cargo.toml", "Cargo.lock"] {
        write_repo_file(&root, path);
    }
    let mut alert = dependabot_alert(71, "serde", "Cargo.toml");
    alert["lockfile_path"] = json!("Cargo.lock");
    let filed = file(&runtime, dependabot_snapshot_for(vec![alert]), None);
    assert_eq!(filed["filed_count"], 1, "{filed}");
    let task = runtime
        .get_task(filed["filed"][0]["task_id"].as_str().unwrap())
        .unwrap();
    assert_eq!(
        task.context_files,
        vec!["file:Cargo.lock".to_string(), "file:Cargo.toml".to_string()],
        "a Dependabot task carries its manifest and the alert-named lockfile"
    );
}

fn sweep_tasks_drop_missing_and_git_orbit_paths() {
    let (root, runtime) = fixture();
    write_repo_file(&root, "crates/a/src/lib.rs");
    write_repo_file(&root, ".git/seeded.rs");
    write_repo_file(&root, ".orbit/seeded.rs");
    let snapshot = code_alerts_snapshot(vec![
        code_alert(484, "rust/cleartext-logging", "crates/a/src/lib.rs", 12),
        code_alert(
            485,
            "rust/cleartext-logging",
            "crates/missing/src/lib.rs",
            12,
        ),
        code_alert(486, "rust/cleartext-logging", ".git/seeded.rs", 12),
        code_alert(487, "rust/cleartext-logging", ".orbit/seeded.rs", 12),
    ]);
    let filed = file(&runtime, snapshot, None);
    assert_eq!(filed["filed_count"], 1, "{filed}");
    let task = runtime
        .get_task(filed["filed"][0]["task_id"].as_str().unwrap())
        .unwrap();
    assert_eq!(
        task.context_files,
        vec!["file:crates/a/src/lib.rs".to_string()],
        "missing and .git/.orbit alert paths are dropped, not filed"
    );

    let (_root, runtime) = fixture();
    let filed = file(
        &runtime,
        dependabot_snapshot_for(vec![dependabot_alert(71, "serde", "missing/Manifest.toml")]),
        None,
    );
    assert_eq!(filed["filed_count"], 1, "{filed}");
    let task = runtime
        .get_task(filed["filed"][0]["task_id"].as_str().unwrap())
        .unwrap();
    assert!(
        task.context_files.is_empty(),
        "a missing manifest seeds no selector: {:?}",
        task.context_files
    );
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
fn rejected_code_scanning_owner_suppresses_refiling_until_the_alert_changes() {
    let (_root, runtime) = fixture();
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
        let (_root, runtime) = fixture();
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
fn rejected_owner_distinguishes_punctuation_only_path_and_rule_changes() {
    let rule = "rust/cleartext-logging";
    let path = "crates/foo-bar/src/lib.rs";

    let (_root, runtime) = fixture();
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
        let (_root, runtime) = fixture();
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
fn rejected_grouped_owner_refills_alert_moved_onto_a_sibling_location() {
    let (_root, runtime) = fixture();
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
fn rejection_naming_a_covering_task_does_not_suppress_code_scanning_refiling() {
    let (_root, runtime) = fixture();
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

fn rejected_source_uses_supersedes_before_legacy_consolidation_comments() {
    for (typed, terminal) in [(true, false), (true, true), (false, false)] {
        let (root, runtime) = fixture();
        let snapshot = || code_snapshot(484, "rust/cleartext-logging", "crates/a/src/lib.rs");
        let first = file(&runtime, snapshot(), None);
        let source_id = first["filed"][0]["task_id"].as_str().unwrap();
        let decoy = runtime
            .add_task(TaskAddParams {
                title: "Unrelated repair".to_string(),
                complexity: TaskComplexity::Medium,
                ..Default::default()
            })
            .unwrap();
        // No copied exact tag or matching prose: only the inbound relation
        // (or the old consolidation comment) can identify this owner.
        let owner = runtime
            .add_task(TaskAddParams {
                title: "Replacement repair".to_string(),
                complexity: TaskComplexity::Medium,
                relations: if typed {
                    vec![TaskRelation {
                        relation_type: TaskRelationType::Supersedes,
                        target: source_id.to_string(),
                        regression: None,
                    }]
                } else {
                    Vec::new()
                },
                ..Default::default()
            })
            .unwrap();
        if terminal {
            runtime
                .reject_task(&owner.id, "No longer covers the alert".to_string(), None)
                .unwrap();
        }
        let named_owner = if typed { &decoy.id } else { &owner.id };
        runtime
            .reject_task(
                source_id,
                format!("Consolidated into covering task {named_owner}"),
                Some(format!(
                    "Consolidated into covering task {named_owner}: the same rule, scanner message, and analysis cover every alert in that group, so one task now owns the shared repair. This task's per-alert evidence and dependencies were carried over."
                )),
            )
            .unwrap();
        assert!(runtime.get_task(source_id).unwrap().relations.is_empty());
        // Open the persisted fixture again, including the legacy record with
        // only the consolidation comment and no Supersedes relation.
        drop(runtime);
        let runtime = OrbitRuntime::from_roots(
            &root.path().join("home/.orbit"),
            &root.path().join("repo/.orbit"),
        )
        .unwrap();
        let repeated = file(&runtime, snapshot(), None);
        if terminal {
            assert_eq!(repeated["filed_count"], 1, "{repeated}");
            assert_eq!(repeated["skipped_existing"], json!([]), "{repeated}");
        } else {
            assert_eq!(repeated["filed_count"], 0, "{repeated}");
            assert_eq!(repeated["skipped_existing"][0]["task_id"], owner.id);
            assert_eq!(
                repeated["skipped_existing"][0]["match_kind"],
                "confirmed_duplicate"
            );
        }
    }
}

fn typed_evidence(number: u64, line: u64) -> CodeScanningAlertEvidence {
    CodeScanningAlertEvidence {
        repository: "acme/orbit".to_string(),
        number,
        rule_id: "rust/example".to_string(),
        rule_name: "Unsafe input".to_string(),
        security_severity: "high".to_string(),
        tool_name: "CodeQL".to_string(),
        tool_version: "1".to_string(),
        tool_guid: "rust".to_string(),
        message: "Unsafe input".to_string(),
        git_ref: "refs/heads/main".to_string(),
        commit_sha: "fixture".to_string(),
        path: "src/lib.rs".to_string(),
        start_line: Some(line),
        end_line: Some(line),
        created_at: String::new(),
        updated_at: String::new(),
        html_url: String::new(),
    }
}

/// New per-alert tasks carry typed evidence on their creation history.
/// Consolidation matches on that struct, so edited description prose does
/// not change grouping, while a mismatched creation decision never falls
/// through to the legacy markdown reader.
fn typed_code_scanning_evidence_consolidates_despite_edited_prose() {
    // Filing a single alert records the typed decision and renders the
    // same human-readable evidence bullets from it.
    let (_root, runtime) = fixture();
    let filed = file(
        &runtime,
        code_snapshot(484, "rust/cleartext-logging", "crates/a/src/lib.rs"),
        None,
    );
    assert_eq!(filed["filed_count"], 1, "{filed}");
    let filed_id = filed["filed"][0]["task_id"].as_str().unwrap().to_string();
    let history = runtime.get_task_history(&filed_id).unwrap();
    let created = history
        .iter()
        .find(|entry| entry.event == "created")
        .unwrap();
    let Some(TaskDecision::CodeScanningAlert(evidence)) = &created.decision else {
        panic!(
            "filed task must carry typed evidence, got {:?}",
            created.decision
        );
    };
    assert_eq!(evidence.repository, "acme/orbit");
    assert_eq!(evidence.number, 484);
    assert_eq!(evidence.rule_id, "rust/cleartext-logging");
    assert_eq!(evidence.path, "crates/a/src/lib.rs");
    assert_eq!(evidence.start_line, Some(12));
    assert_eq!(evidence.git_ref, "refs/heads/main");
    let filed_task = runtime.get_task(&filed_id).unwrap();
    assert!(
        filed_task.description.contains(
            "## Alert evidence\n\n- Repository: `acme/orbit`\n- Alert: `#484`\n- Rule: `rust/cleartext-logging` (Cleartext logging)\n- Security severity: `high`\n- Tool: `CodeQL` (version `2`, guid `codeql`)\n- Message: Logs sensitive data\n- Ref: `refs/heads/main`\n- Commit: `fixture`\n- Location: `crates/a/src/lib.rs` line 12\n- Created: `unknown`\n- Updated: `unknown`\n- Alert URL: unknown\n"
        ),
        "rendered description must stay byte-identical:\n{}",
        filed_task.description
    );

    // Typed records whose prose was edited past recognition still group.
    let (_root, runtime) = fixture();
    let mut source_ids = Vec::new();
    for number in [81_u64, 82] {
        let source = runtime
            .add_task(TaskAddParams {
                title: format!("Typed alert {number}"),
                description: "Edited by an operator; the evidence block is gone.".to_string(),
                tags: vec![
                    "code-scanning-sweep".to_string(),
                    format!("code-scanning:fixture-{number}"),
                ],
                creation_decision: Some(TaskDecision::CodeScanningAlert(typed_evidence(
                    number, number,
                ))),
                complexity: TaskComplexity::Medium,
                status: Some(TaskStatus::Backlog),
                ..Default::default()
            })
            .unwrap();
        source_ids.push(source.id);
    }
    source_ids.sort();
    let consolidate = |apply| {
        orbit_core::test_support::EngineHost::new(&runtime)
            .run_deterministic(
                "consolidate_code_scanning_tasks",
                &json!({}),
                &json!({"apply": apply}),
                ToolContext::default(),
            )
            .unwrap()
    };
    let preview = consolidate(false);
    assert_eq!(preview["outcome"], "dry_run", "{preview}");
    assert_eq!(preview["group_count"], 1);
    assert_eq!(preview["groups"][0]["source_task_ids"], json!(source_ids));
    assert_eq!(preview["groups"][0]["alert_numbers"], json!([81, 82]));
    let applied = consolidate(true);
    assert_eq!(applied["outcome"], "applied", "{applied}");
    for id in &source_ids {
        assert_eq!(runtime.get_task(id).unwrap().status, TaskStatus::Rejected);
    }

    // A creation entry carrying a different decision is mismatched: the
    // valid legacy markdown in its description must not be consulted.
    let (_root, runtime) = fixture();
    for number in [81_u64, 82] {
        runtime
            .add_task(TaskAddParams {
                title: format!("Mismatched alert {number}"),
                description: format!(
                    "## Alert evidence\n\n- Repository: `acme/orbit`\n- Alert: `#{number}`\n- Rule: `rust/example` (Unsafe input)\n- Security severity: `high`\n- Tool: `CodeQL` (version `1`, guid `rust`)\n- Message: Unsafe input\n- Ref: `refs/heads/main`\n- Commit: `fixture`\n- Location: `src/lib.rs` line {number}\n"
                ),
                tags: vec![
                    "code-scanning-sweep".to_string(),
                    format!("code-scanning:fixture-{number}"),
                ],
                creation_decision: Some(TaskDecision::DrainApprovalHeld {
                    classification: "fixture".to_string(),
                }),
                complexity: TaskComplexity::Medium,
                status: Some(TaskStatus::Backlog),
                ..Default::default()
            })
            .unwrap();
    }
    let refused = consolidate_on(&runtime);
    assert_eq!(refused["group_count"], 0, "{refused}");
    assert_eq!(refused["scanned_source_tasks"], 0);
    let reasons: Vec<_> = refused["skipped"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["reason"].as_str().unwrap())
        .collect();
    assert_eq!(reasons, vec!["unparsable_evidence", "unparsable_evidence"]);
}

fn consolidate_on(runtime: &OrbitRuntime) -> Value {
    orbit_core::test_support::EngineHost::new(runtime)
        .run_deterministic(
            "consolidate_code_scanning_tasks",
            &json!({}),
            &json!({"apply": false}),
            ToolContext::default(),
        )
        .unwrap()
}

fn mixed_legacy_and_typed_code_scanning_evidence_with_missing_guid_consolidates() {
    let (_root, runtime) = fixture();

    // 1. Mixed pair with missing GUID (alert 81 is legacy with guid `unknown`,
    //    alert 82 is typed with empty guid ""). Both share the same cause:
    //    rule rust/example, message "Unsafe input", tool CodeQL v1, ref refs/heads/main.
    let legacy_missing_guid = runtime
        .add_task(TaskAddParams {
            title: "Legacy alert #81 missing guid".to_string(),
            description: "## Alert evidence\n\n\
                 - Repository: `acme/orbit`\n\
                 - Alert: `#81`\n\
                 - Rule: `rust/example` (Unsafe input)\n\
                 - Security severity: `high`\n\
                 - Tool: `CodeQL` (version `1`, guid `unknown`)\n\
                 - Message: Unsafe input\n\
                 - Ref: `refs/heads/main`\n\
                 - Commit: `fixture`\n\
                 - Location: `src/lib.rs` line 81\n"
                .to_string(),
            tags: vec![
                "code-scanning-sweep".to_string(),
                "code-scanning:fixture-81".to_string(),
            ],
            creation_decision: None,
            complexity: TaskComplexity::Medium,
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .unwrap();

    let typed_missing_guid = runtime
        .add_task(TaskAddParams {
            title: "Typed alert #82 missing guid".to_string(),
            description: "Prose does not matter for typed decision".to_string(),
            tags: vec![
                "code-scanning-sweep".to_string(),
                "code-scanning:fixture-82".to_string(),
            ],
            creation_decision: Some(TaskDecision::CodeScanningAlert(CodeScanningAlertEvidence {
                repository: "acme/orbit".to_string(),
                number: 82,
                rule_id: "rust/example".to_string(),
                rule_name: "Unsafe input".to_string(),
                security_severity: "high".to_string(),
                tool_name: "CodeQL".to_string(),
                tool_version: "1".to_string(),
                tool_guid: String::new(),
                message: "Unsafe input".to_string(),
                git_ref: "refs/heads/main".to_string(),
                commit_sha: "fixture".to_string(),
                path: "src/lib.rs".to_string(),
                start_line: Some(82),
                end_line: Some(82),
                created_at: String::new(),
                updated_at: String::new(),
                html_url: String::new(),
            })),
            complexity: TaskComplexity::Medium,
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .unwrap();

    // 2. Control: Distinct scanner identity with a populated guid "distinct-scanner".
    //    Same rule and message, but must not consolidate with the missing-guid cause.
    let distinct_scanner = runtime
        .add_task(TaskAddParams {
            title: "Typed alert #83 distinct scanner".to_string(),
            description: "Distinct scanner guid".to_string(),
            tags: vec![
                "code-scanning-sweep".to_string(),
                "code-scanning:fixture-83".to_string(),
            ],
            creation_decision: Some(TaskDecision::CodeScanningAlert(CodeScanningAlertEvidence {
                repository: "acme/orbit".to_string(),
                number: 83,
                rule_id: "rust/example".to_string(),
                rule_name: "Unsafe input".to_string(),
                security_severity: "high".to_string(),
                tool_name: "CodeQL".to_string(),
                tool_version: "1".to_string(),
                tool_guid: "distinct-scanner".to_string(),
                message: "Unsafe input".to_string(),
                git_ref: "refs/heads/main".to_string(),
                commit_sha: "fixture".to_string(),
                path: "src/lib.rs".to_string(),
                start_line: Some(83),
                end_line: Some(83),
                created_at: String::new(),
                updated_at: String::new(),
                html_url: String::new(),
            })),
            complexity: TaskComplexity::Medium,
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .unwrap();

    // 3. Control: Present-GUID pair (one legacy, one typed, both with guid "rust").
    let legacy_present_guid = runtime
        .add_task(TaskAddParams {
            title: "Legacy alert #84 present guid".to_string(),
            description: "## Alert evidence\n\n\
                 - Repository: `acme/orbit`\n\
                 - Alert: `#84`\n\
                 - Rule: `rust/example` (Unsafe input)\n\
                 - Security severity: `high`\n\
                 - Tool: `CodeQL` (version `1`, guid `rust`)\n\
                 - Message: Unsafe input\n\
                 - Ref: `refs/heads/main`\n\
                 - Commit: `fixture`\n\
                 - Location: `src/lib.rs` line 84\n"
                .to_string(),
            tags: vec![
                "code-scanning-sweep".to_string(),
                "code-scanning:fixture-84".to_string(),
            ],
            creation_decision: None,
            complexity: TaskComplexity::Medium,
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .unwrap();

    let typed_present_guid = runtime
        .add_task(TaskAddParams {
            title: "Typed alert #85 present guid".to_string(),
            description: "Present guid typed".to_string(),
            tags: vec![
                "code-scanning-sweep".to_string(),
                "code-scanning:fixture-85".to_string(),
            ],
            creation_decision: Some(TaskDecision::CodeScanningAlert(typed_evidence(85, 85))),
            complexity: TaskComplexity::Medium,
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .unwrap();

    // 4. Control: Legacy-only missing GUID pair (alerts #86 and #87).
    let legacy_missing_86 = runtime
        .add_task(TaskAddParams {
            title: "Legacy alert #86 missing guid".to_string(),
            description: "## Alert evidence\n\n\
                 - Repository: `acme/orbit`\n\
                 - Alert: `#86`\n\
                 - Rule: `rust/other` (Other issue)\n\
                 - Security severity: `high`\n\
                 - Tool: `CodeQL` (version `1`, guid `unknown`)\n\
                 - Message: Other issue\n\
                 - Ref: `refs/heads/main`\n\
                 - Commit: `fixture`\n\
                 - Location: `src/lib.rs` line 86\n"
                .to_string(),
            tags: vec![
                "code-scanning-sweep".to_string(),
                "code-scanning:fixture-86".to_string(),
            ],
            creation_decision: None,
            complexity: TaskComplexity::Medium,
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .unwrap();

    let legacy_missing_87 = runtime
        .add_task(TaskAddParams {
            title: "Legacy alert #87 missing guid".to_string(),
            description: "## Alert evidence\n\n\
                 - Repository: `acme/orbit`\n\
                 - Alert: `#87`\n\
                 - Rule: `rust/other` (Other issue)\n\
                 - Security severity: `high`\n\
                 - Tool: `CodeQL` (version `1`, guid `unknown`)\n\
                 - Message: Other issue\n\
                 - Ref: `refs/heads/main`\n\
                 - Commit: `fixture`\n\
                 - Location: `src/lib.rs` line 87\n"
                .to_string(),
            tags: vec![
                "code-scanning-sweep".to_string(),
                "code-scanning:fixture-87".to_string(),
            ],
            creation_decision: None,
            complexity: TaskComplexity::Medium,
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .unwrap();

    let consolidate = |apply| {
        orbit_core::test_support::EngineHost::new(&runtime)
            .run_deterministic(
                "consolidate_code_scanning_tasks",
                &json!({}),
                &json!({"apply": apply}),
                ToolContext::default(),
            )
            .unwrap()
    };

    let preview = consolidate(false);
    assert_eq!(preview["outcome"], "dry_run", "{preview}");
    assert_eq!(preview["scanned_source_tasks"], 7);
    assert_eq!(preview["group_count"], 3, "{preview}");

    // The distinct scanner identity remains a sole task for its cause.
    let unchanged: Vec<_> = preview["unchanged_single_source"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["task_id"].as_str().unwrap())
        .collect();
    assert_eq!(unchanged, vec![distinct_scanner.id.as_str()]);

    let mut missing_guid_ids = vec![
        legacy_missing_guid.id.clone(),
        typed_missing_guid.id.clone(),
    ];
    missing_guid_ids.sort();
    let mut present_guid_ids = vec![
        legacy_present_guid.id.clone(),
        typed_present_guid.id.clone(),
    ];
    present_guid_ids.sort();
    let mut legacy_only_ids = vec![legacy_missing_86.id.clone(), legacy_missing_87.id.clone()];
    legacy_only_ids.sort();

    let groups = preview["groups"].as_array().unwrap();
    let find_group = |ids: &[String]| {
        groups.iter().find(|group| {
            let mut source_ids: Vec<String> = group["source_task_ids"]
                .as_array()
                .unwrap()
                .iter()
                .map(|id| id.as_str().unwrap().to_string())
                .collect();
            source_ids.sort();
            source_ids == ids
        })
    };

    let missing_guid_group = find_group(&missing_guid_ids)
        .expect("missing-guid mixed pair must consolidate into one group");
    assert_eq!(missing_guid_group["alert_numbers"], json!([81, 82]));

    let present_guid_group = find_group(&present_guid_ids)
        .expect("present-guid mixed pair must consolidate into one group");
    assert_eq!(present_guid_group["alert_numbers"], json!([84, 85]));

    let legacy_only_group = find_group(&legacy_only_ids)
        .expect("legacy-only missing-guid pair must consolidate into one group");
    assert_eq!(legacy_only_group["alert_numbers"], json!([86, 87]));

    // Apply consolidation and verify both owners in each group are retired
    // and carried into the replacement.
    let applied = consolidate(true);
    assert_eq!(applied["outcome"], "applied", "{applied}");
    assert_eq!(applied["group_count"], 3);

    assert_eq!(
        runtime.get_task(&legacy_missing_guid.id).unwrap().status,
        TaskStatus::Rejected
    );
    assert_eq!(
        runtime.get_task(&typed_missing_guid.id).unwrap().status,
        TaskStatus::Rejected
    );
    assert_eq!(
        runtime.get_task(&distinct_scanner.id).unwrap().status,
        TaskStatus::Backlog
    );
    assert_eq!(
        runtime.get_task(&legacy_present_guid.id).unwrap().status,
        TaskStatus::Rejected
    );
    assert_eq!(
        runtime.get_task(&typed_present_guid.id).unwrap().status,
        TaskStatus::Rejected
    );
    assert_eq!(
        runtime.get_task(&legacy_missing_86.id).unwrap().status,
        TaskStatus::Rejected
    );
    assert_eq!(
        runtime.get_task(&legacy_missing_87.id).unwrap().status,
        TaskStatus::Rejected
    );
}

fn held_code_alert(number: u64, rule: &str, path: &str, message: &str) -> Value {
    let mut alert = code_alert(number, rule, path, 12);
    alert["tool_version"] = json!("2.27.5");
    alert["message"] = json!(message);
    alert
}

fn drift_frictions(runtime: &OrbitRuntime) -> Vec<Value> {
    let response = runtime
        .execute_tool_command_dispatch_with_session_context(
            "orbit.attention",
            json!({"kind": "friction_untriaged"}),
            None,
            None,
            ToolEntryPoint::Mcp,
            ToolSessionContext {
                effective_capabilities: [McpCapability::Operator].into(),
                ..Default::default()
            },
        )
        .unwrap()
        .value;
    response["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| {
            item["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("CodeQL bundle drift"))
        })
        .cloned()
        .collect()
}

fn task_ids_tagged(runtime: &OrbitRuntime, tag: &str) -> Vec<String> {
    let mut ids = runtime
        .list_tasks()
        .unwrap()
        .into_iter()
        .filter(|task| task.tags.iter().any(|candidate| candidate == tag))
        .map(|task| task.id.to_string())
        .collect::<Vec<_>>();
    ids.sort();
    ids
}

/// ORB-26147: hosted CodeQL 2.27.5 raised `rust/unsafe-deserialization`,
/// which the host's 2.27.1 bundle does not offer; the sweep filed a drainable
/// repair that could only block `validation_unavailable`.
fn code_scanning_rule_missing_from_installed_bundle_waits_on_one_prerequisite() {
    let (_root, runtime) = fixture();
    let rule = "rust/unsafe-deserialization";
    let snapshot = code_alerts_snapshot(vec![
        code_alert(484, "rust/cleartext-logging", "crates/a/src/lib.rs", 12),
        held_code_alert(
            505,
            rule,
            "crates/a/src/lib.rs",
            "Deserializes untrusted data",
        ),
        held_code_alert(
            506,
            rule,
            "crates/b/src/lib.rs",
            "Deserializes request bodies",
        ),
    ]);
    let first = file(&runtime, snapshot.clone(), None);
    assert_eq!(first["filed_count"], 3, "{first}");
    let prerequisites = first["codeql_bundle_prerequisites"].as_array().unwrap();
    assert_eq!(
        prerequisites.len(),
        1,
        "one prerequisite per host and version: {first}"
    );
    assert_eq!(prerequisites[0]["created"], true);
    assert_eq!(prerequisites[0]["hosted_version"], "2.27.5");
    assert_eq!(prerequisites[0]["installed_version"], "2.27.1");
    let prerequisite_id = prerequisites[0]["task_id"].as_str().unwrap().to_string();
    let prerequisite = runtime.get_task(&prerequisite_id).unwrap();
    assert_eq!(prerequisite.status, TaskStatus::Proposed);
    assert!(
        prerequisite.title.contains("CodeQL bundle 2.27.5"),
        "{}",
        prerequisite.title
    );

    let filed = first["filed"].as_array().unwrap();
    for entry in filed {
        let task = runtime
            .get_task(entry["task_id"].as_str().unwrap())
            .unwrap();
        if entry["rule_id"] == rule {
            assert_eq!(entry["held_by"], prerequisite_id.as_str(), "{entry}");
            assert_eq!(
                task.dependencies()
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>(),
                vec![prerequisite_id.clone()],
                "a held repair is not drainable until its prerequisite is done"
            );
        } else {
            // The rule the bundle offers files exactly as before.
            assert!(entry.get("held_by").is_none(), "{entry}");
            assert!(entry.get("held_for_codeql_bundle").is_none(), "{entry}");
            assert!(task.dependencies().is_empty());
            assert!(
                !task
                    .description
                    .contains("Waiting on the local CodeQL bundle")
            );
            assert_eq!(task.status, TaskStatus::Backlog);
        }
    }

    let drift = drift_frictions(&runtime);
    assert_eq!(drift.len(), 1, "{drift:?}");
    let reason = drift[0]["reason"].as_str().unwrap();
    for named in [
        "2.27.5",
        "2.27.1",
        prerequisites[0]["host"].as_str().unwrap(),
    ] {
        assert!(reason.contains(named), "the drift names {named}: {reason}");
    }

    // A second sweep over the same alerts files nothing new.
    let tasks_before = runtime.list_tasks().unwrap().len();
    let second = file(&runtime, snapshot, None);
    assert_eq!(second["filed_count"], 0, "{second}");
    assert_eq!(second["codeql_bundle_prerequisites"], json!([]), "{second}");
    assert_eq!(runtime.list_tasks().unwrap().len(), tasks_before);
    assert_eq!(drift_frictions(&runtime).len(), 1);

    // A later alert of a missing rule joins the same prerequisite.
    let third = file(
        &runtime,
        code_alerts_snapshot(vec![held_code_alert(
            507,
            "rust/another-new-query",
            "crates/c/src/lib.rs",
            "New finding",
        )]),
        None,
    );
    assert_eq!(third["filed_count"], 1, "{third}");
    assert_eq!(third["codeql_bundle_prerequisites"][0]["created"], false);
    assert_eq!(third["filed"][0]["held_by"], prerequisite_id.as_str());
    assert_eq!(
        task_ids_tagged(&runtime, "codeql-bundle-prerequisite"),
        vec![prerequisite_id]
    );
    assert_eq!(drift_frictions(&runtime).len(), 1);
}

/// With no CodeQL CLI on the owner's validation PATH, every CodeQL repair is
/// held the way a missing rule is.
fn code_scanning_without_installed_bundle_waits_on_a_prerequisite() {
    let (root, runtime) = fixture();
    drop(runtime);
    let empty = root.path().join("no-codeql");
    std::fs::create_dir_all(&empty).unwrap();
    std::fs::write(root.path().join("repo/.orbit/config.toml"), "").unwrap();
    let runtime = OrbitRuntime::from_roots(
        &root.path().join("home/.orbit"),
        &root.path().join("repo/.orbit"),
    )
    .unwrap();
    let path = format!("{}:/usr/bin:/bin", empty.display());
    let _env = orbit_common::test_env::scoped([("PATH", Some(path.as_str()))]);

    let mut alert = code_alert(484, "rust/cleartext-logging", "crates/a/src/lib.rs", 12);
    alert["tool_version"] = json!("2.27.5");
    let first = file(
        &orbit_core::test_support::with_runtime_env(runtime.clone(), orbit_common::Env::capture()),
        code_alerts_snapshot(vec![alert.clone()]),
        None,
    );
    assert_eq!(first["filed_count"], 1, "{first}");
    let prerequisites = first["codeql_bundle_prerequisites"].as_array().unwrap();
    assert_eq!(prerequisites.len(), 1, "{first}");
    assert_eq!(prerequisites[0]["installed_version"], Value::Null);
    let prerequisite_id = prerequisites[0]["task_id"].as_str().unwrap();
    assert_eq!(first["filed"][0]["held_by"], prerequisite_id);
    let drift = drift_frictions(&runtime);
    assert_eq!(drift.len(), 1, "{drift:?}");
    assert!(
        drift[0]["reason"]
            .as_str()
            .unwrap()
            .contains("none installed"),
        "{drift:?}"
    );

    let second = file(
        &orbit_core::test_support::with_runtime_env(runtime.clone(), orbit_common::Env::capture()),
        code_alerts_snapshot(vec![alert]),
        None,
    );
    assert_eq!(second["filed_count"], 0, "{second}");
    assert_eq!(drift_frictions(&runtime).len(), 1);
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn init_code_scanning_git_history(root: &Path) -> CodeScanningHistory {
    let repo = root.join("repo");
    git(&repo, &["init", "-b", "agent-main"]);
    git(&repo, &["config", "user.name", "Fixture"]);
    git(&repo, &["config", "user.email", "fixture@example.invalid"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);

    std::fs::write(repo.join("lib.rs"), "pub fn base() {}\n").unwrap();
    git(&repo, &["add", "lib.rs"]);
    git(&repo, &["commit", "-qm", "pre-fix base"]);
    let pre_fix_sha = git(&repo, &["rev-parse", "HEAD"]);

    git(&repo, &["checkout", "-qb", "diverged"]);
    std::fs::write(repo.join("diverged.rs"), "pub fn other() {}\n").unwrap();
    git(&repo, &["add", "diverged.rs"]);
    git(&repo, &["commit", "-qm", "diverged commit"]);
    let diverged_sha = git(&repo, &["rev-parse", "HEAD"]);

    git(&repo, &["checkout", "-q", "agent-main"]);
    let mut landed = Vec::new();
    for index in 1..=3 {
        std::fs::write(repo.join(format!("fix_{index}.rs")), "pub fn fix() {}\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-qm", &format!("landed fix {index}")]);
        landed.push(git(&repo, &["rev-parse", "HEAD"]));
    }
    std::fs::write(repo.join("later.rs"), "pub fn later() {}\n").unwrap();
    git(&repo, &["add", "later.rs"]);
    git(&repo, &["commit", "-qm", "post-fix descendant"]);
    let post_fix_sha = git(&repo, &["rev-parse", "HEAD"]);

    CodeScanningHistory {
        pre_fix_sha,
        landed_shas: landed.try_into().unwrap(),
        post_fix_sha,
        diverged_sha,
    }
}

fn code_alert_at(
    number: u64,
    rule: &str,
    path: &str,
    line: u64,
    git_ref: &str,
    commit_sha: &str,
) -> Value {
    let mut alert = code_alert(number, rule, path, line);
    alert["ref"] = json!(git_ref);
    alert["commit_sha"] = json!(commit_sha);
    alert["message"] = json!(format!("Finding for {rule}"));
    alert
}

fn orbit_code_snapshot(repository: &str, alerts: Vec<Value>) -> Value {
    let mut snapshot = code_alerts_snapshot(alerts);
    snapshot["repository"] = json!({"full_name": repository});
    snapshot
}

fn settle_task_external_merge(
    runtime: &OrbitRuntime,
    task_id: &str,
    pr_number: u64,
    branch: &str,
    merge_commit: &str,
    execution_summary: &str,
) {
    orbit_core::test_support::EngineHost::new(runtime)
        .apply_task_automation_update(
            task_id,
            orbit_engine::TaskAutomationUpdate {
                status: Some(TaskStatus::Done),
                execution_summary: Some(execution_summary.to_string()),
                status_event: Some(EXTERNAL_MERGE_SETTLED_EVENT.to_string()),
                status_note: Some(format!(
                    "settled from pull request #{pr_number}, merged into {branch} at 2026-11-27T00:33:03Z as {merge_commit} outside Orbit"
                )),
                ..Default::default()
            },
        )
        .unwrap();
}

fn install_pr_delivery_run(
    runtime: &OrbitRuntime,
    task_id: &str,
    base_branch: &str,
    landed_commit: &str,
    no_change: bool,
) {
    let resources = runtime.paths().global_dir.join("resources");
    std::fs::create_dir_all(resources.join("activities")).unwrap();
    std::fs::create_dir_all(resources.join("jobs")).unwrap();
    for activity in ["git_commit", "pr_complete"] {
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(format!("assets/activities/{activity}.yaml")),
            resources.join(format!("activities/{activity}.yaml")),
        )
        .unwrap();
    }
    std::fs::write(
        resources.join("jobs/fixture_delivery.yaml"),
        "schemaVersion: 2\nkind: Job\nmetadata:\n  name: fixture_delivery\nspec:\n  \
         state: enabled\n  task_delivery: {}\n  steps:\n    - id: commit\n      \
         target: activity:git_commit\n    - id: complete_pr\n      target: activity:pr_complete\n",
    )
    .unwrap();
    let mut run = orbit_core::test_support::EngineHost::new(runtime)
        .insert_job_run(
            "fixture_delivery",
            1,
            chrono::Utc::now(),
            Some(json!({"task_ids": [task_id]})),
            None,
        )
        .unwrap();
    run.state = JobRunState::Success;
    runtime
        .sqlite_store()
        .unwrap()
        .write(|tx| {
            tx.job_run()
                .put(&runtime.workspace_id().unwrap(), &run, None)
        })
        .unwrap();
    let commit_output = if no_change {
        json!({
            "phase": "commit",
            "decision": "verified_no_diff",
            "committed": false,
            "job_run_id": run.run_id,
            "task_id": task_id,
        })
    } else {
        json!({
            "phase": "commit",
            "decision": "performed",
            "committed": true,
            "commit_sha": landed_commit,
            "base_sha": "a".repeat(40),
            "job_run_id": run.run_id,
            "task_id": task_id,
        })
    };
    let mut state = PipelineState::new(run.run_id.clone(), run.job_id.clone(), json!({}));
    for (index, step_id, output) in [
        (0u32, "commit", commit_output),
        (
            1,
            "complete_pr",
            json!({
                "phase": "complete",
                "merge": {"merged": true, "pr_number": "11490", "landed_commit": landed_commit},
                "delivery_evidence": {
                    "pr_number": "11490",
                    "base_ref": base_branch,
                    "merge_commit": landed_commit,
                },
            }),
        ),
    ] {
        state.record_step(index, JobRunState::Success, Some(output.clone()), None);
        state.record_pipeline_output(step_id, output);
    }
    runtime.write_run_state(&run.run_id, &state).unwrap();
}

/// ORB-29223: Replay of the 18 pre-fix alert instances across 3 landed repairs
/// files no duplicate tasks and reports every alert as awaiting a post-fix scan.
/// Also verifies run-delivery (`pr_complete`) and `landing_completed` evidence.
fn pre_fix_code_scanning_replay_defers_all_18_alerts_until_post_fix_scan() {
    let (root, runtime) = fixture();
    let history = init_code_scanning_git_history(root.path());
    let repo_name = "constellation-works/orbit";
    let git_ref = "refs/heads/agent-main";

    let mut group_1 = Vec::new();
    for (idx, number) in (507..=508).enumerate() {
        group_1.push(code_alert_at(
            number,
            "rust/cleartext-logging",
            "crates/orbit-types/src/task.rs",
            417 + idx as u64 * 17,
            git_ref,
            &history.pre_fix_sha,
        ));
    }
    let mut group_2 = Vec::new();
    for (idx, number) in (509..=516).enumerate() {
        group_2.push(code_alert_at(
            number,
            "rust/path-injection",
            &format!("crates/orbit-store/src/file_{}.rs", idx / 3),
            70 + idx as u64 * 10,
            git_ref,
            &history.pre_fix_sha,
        ));
    }
    let mut group_3 = Vec::new();
    for (idx, number) in (517..=524).enumerate() {
        group_3.push(code_alert_at(
            number,
            "rust/example",
            &format!("crates/orbit-common/src/lock_{}.rs", idx / 2),
            210 + idx as u64 * 12,
            git_ref,
            &history.pre_fix_sha,
        ));
    }
    let mut all_18 = Vec::new();
    all_18.extend(group_1);
    all_18.extend(group_2);
    all_18.extend(group_3);

    let initial = file(
        &runtime,
        orbit_code_snapshot(repo_name, all_18.clone()),
        None,
    );
    assert_eq!(initial["filed_count"], 3, "{initial}");
    let task_ids: Vec<String> = initial["filed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["task_id"].as_str().unwrap().to_string())
        .collect();

    // Settle the three grouped tasks with the three real summary disposition formats.
    settle_task_external_merge(
        &runtime,
        &task_ids[0],
        11408,
        "agent-main",
        &history.landed_shas[0],
        "## Alert dispositions\n\n\
         - #507 (line 417): fixed (source side). Hosted alert NOT closed.\n\
         - #508 (line 434): fixed (source side). Same caveat.\n\
         - Not already-covered and not not-reproducible-in-source.",
    );
    let summary_2 = (509..=516)
        .map(|number| {
            format!("- Alert #{number} (`bundle.rs:{number}`, `read`): `fixed` — validated path.")
        })
        .collect::<Vec<_>>()
        .join("\n");
    settle_task_external_merge(
        &runtime,
        &task_ids[1],
        11410,
        "agent-main",
        &history.landed_shas[1],
        &format!(
            "Outcome: success\n\nPer-alert disposition (source-side coverage; hosted alerts remain open):\n{summary_2}\n\nValidation:\n- passed"
        ),
    );
    let summary_3 = (517..=524)
        .map(|number| {
            format!(
                "- Alert `#{number}` (`crates/orbit-common/src/lock.rs:{number}`): `fixed` (source-side)."
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    settle_task_external_merge(
        &runtime,
        &task_ids[2],
        11411,
        "agent-main",
        &history.landed_shas[2],
        &format!(
            "Outcome: success\n\nPer-alert disposition (requires hosted rescan):\n{summary_3}"
        ),
    );

    // Seed an archived duplicate carrying the same exact tags (like ORB-29173).
    let archived = runtime
        .add_task(TaskAddParams {
            title: "Archived duplicate".to_string(),
            tags: runtime.get_task(&task_ids[0]).unwrap().tags,
            complexity: TaskComplexity::Low,
            status: Some(TaskStatus::Backlog),
            ..Default::default()
        })
        .unwrap();
    orbit_core::test_support::EngineHost::new(&runtime)
        .apply_task_automation_update(
            &archived.id,
            orbit_engine::TaskAutomationUpdate {
                status: Some(TaskStatus::Archived),
                status_note: Some("duplicate archived".to_string()),
                ..Default::default()
            },
        )
        .unwrap();

    let replay = file(&runtime, orbit_code_snapshot(repo_name, all_18), None);
    assert_eq!(replay["filed_count"], 0, "{replay}");
    assert_eq!(replay["filed"], json!([]));
    assert_eq!(replay["skipped_existing"], json!([]));
    assert_eq!(replay["clusters"], 18, "{replay}");
    let awaiting = replay["awaiting_post_fix_scan"].as_array().unwrap();
    assert_eq!(awaiting.len(), 18, "{replay}");
    for (idx, number) in (507_u64..=524).enumerate() {
        let entry = &awaiting[idx];
        let expected_idx = match number {
            507..=508 => 0,
            509..=516 => 1,
            _ => 2,
        };
        assert_eq!(entry["family"], "code_scanning");
        assert_eq!(entry["alert_number"], number);
        assert_eq!(entry["repository"], repo_name);
        assert_eq!(entry["ref"], git_ref);
        assert_eq!(entry["analyzed_sha"], history.pre_fix_sha);
        assert_eq!(entry["covering_task_id"], task_ids[expected_idx]);
        assert_eq!(entry["landed_sha"], history.landed_shas[expected_idx]);
        assert_eq!(entry["disposition"], "fixed");
        assert_eq!(entry["status"], "awaiting_post_fix_scan");
    }

    // Verify single-alert typed tasks also defer under pr_complete run delivery
    // and landing_completed history notes.
    let single_alerts = vec![
        code_alert_at(
            601,
            "rust/cleartext-logging",
            "crates/a.rs",
            10,
            git_ref,
            &history.pre_fix_sha,
        ),
        code_alert_at(
            602,
            "rust/path-injection",
            "crates/b.rs",
            20,
            git_ref,
            &history.pre_fix_sha,
        ),
    ];
    let single_filed = file(
        &runtime,
        orbit_code_snapshot(repo_name, single_alerts.clone()),
        None,
    );
    assert_eq!(single_filed["filed_count"], 2, "{single_filed}");
    let single_a = single_filed["filed"][0]["task_id"].as_str().unwrap();
    let single_b = single_filed["filed"][1]["task_id"].as_str().unwrap();

    orbit_core::test_support::EngineHost::new(&runtime)
        .apply_task_automation_update(
            single_a,
            orbit_engine::TaskAutomationUpdate {
                status: Some(TaskStatus::Done),
                execution_summary: Some("## Per-alert disposition\n- #601: `fixed`".to_string()),
                status_note: Some("completed".to_string()),
                ..Default::default()
            },
        )
        .unwrap();
    install_pr_delivery_run(
        &runtime,
        single_a,
        "agent-main",
        &history.landed_shas[0],
        false,
    );

    orbit_core::test_support::EngineHost::new(&runtime)
        .apply_task_automation_update(
            single_b,
            orbit_engine::TaskAutomationUpdate {
                status: Some(TaskStatus::Done),
                execution_summary: Some(
                    "## Per-alert disposition\n- #602: `already-covered`".to_string(),
                ),
                status_note: Some("completed".to_string()),
                append_history: vec![TaskHistoryEntry {
                    at: chrono::Utc::now(),
                    by: "system".to_string(),
                    event: "landing_completed".to_string(),
                    note: Some(
                        json!({
                            "reconciled": "local_target_contains_candidate",
                            "landing_ref": "refs/heads/agent-main",
                            "candidate": history.landed_shas[1],
                        })
                        .to_string(),
                    ),
                    from_status: None,
                    to_status: None,
                    operation_id: None,
                    decision: None,
                    surface: None,
                    caller: None,
                }],
                ..Default::default()
            },
        )
        .unwrap();

    let single_replay = file(
        &runtime,
        orbit_code_snapshot(repo_name, single_alerts),
        None,
    );
    assert_eq!(single_replay["filed_count"], 0, "{single_replay}");
    let single_awaiting = single_replay["awaiting_post_fix_scan"].as_array().unwrap();
    assert_eq!(single_awaiting.len(), 2, "{single_replay}");
    assert_eq!(single_awaiting[0]["covering_task_id"], single_a);
    assert_eq!(single_awaiting[1]["covering_task_id"], single_b);
    assert_eq!(single_awaiting[1]["disposition"], "already-covered");
}

/// A later scan containing the landed fix (either at landed_sha or a descendant)
/// and any new alert on the pre-fix scan are eligible for remediation.
fn post_fix_code_scanning_scan_and_new_alerts_remain_eligible() {
    let (root, runtime) = fixture();
    let history = init_code_scanning_git_history(root.path());
    let repo_name = "constellation-works/orbit";
    let git_ref = "refs/heads/agent-main";

    let initial = file(
        &runtime,
        orbit_code_snapshot(
            repo_name,
            vec![
                code_alert_at(
                    507,
                    "rust/cleartext-logging",
                    "crates/a.rs",
                    12,
                    git_ref,
                    &history.pre_fix_sha,
                ),
                code_alert_at(
                    508,
                    "rust/cleartext-logging",
                    "crates/b.rs",
                    24,
                    git_ref,
                    &history.pre_fix_sha,
                ),
            ],
        ),
        None,
    );
    assert_eq!(initial["filed_count"], 1, "{initial}");
    let owner = initial["filed"][0]["task_id"].as_str().unwrap().to_string();
    settle_task_external_merge(
        &runtime,
        &owner,
        11408,
        "agent-main",
        &history.landed_shas[0],
        "## Per-alert disposition\n- #507: `fixed`\n- #508: `fixed`",
    );

    // 1. Scan at landed_sha (#507) and descendant (#508): both are re-filed!
    let post_fix = file(
        &runtime,
        orbit_code_snapshot(
            repo_name,
            vec![
                code_alert_at(
                    507,
                    "rust/cleartext-logging",
                    "crates/a.rs",
                    12,
                    git_ref,
                    &history.landed_shas[0],
                ),
                code_alert_at(
                    508,
                    "rust/cleartext-logging",
                    "crates/b.rs",
                    24,
                    git_ref,
                    &history.post_fix_sha,
                ),
            ],
        ),
        None,
    );
    assert_eq!(post_fix["filed_count"], 1, "{post_fix}");
    assert_eq!(post_fix["awaiting_post_fix_scan"], json!([]));
    assert_eq!(post_fix["filed"][0]["alert_numbers"], json!([507, 508]));

    // 2. New alert #599 on pre_fix_sha is filed while #507 on pre_fix_sha is
    // deferred under `done_owner` (and #599 is not told open work covers #507).
    let (root, runtime) = fixture();
    let history = init_code_scanning_git_history(root.path());
    let first = file(
        &runtime,
        orbit_code_snapshot(
            repo_name,
            vec![code_alert_at(
                507,
                "rust/cleartext-logging",
                "crates/a.rs",
                12,
                git_ref,
                &history.pre_fix_sha,
            )],
        ),
        None,
    );
    let done_owner = first["filed"][0]["task_id"].as_str().unwrap();
    settle_task_external_merge(
        &runtime,
        done_owner,
        11408,
        "agent-main",
        &history.landed_shas[0],
        "## Per-alert disposition\n- #507: `fixed`",
    );
    let mixed = file(
        &runtime,
        orbit_code_snapshot(
            repo_name,
            vec![
                code_alert_at(
                    507,
                    "rust/cleartext-logging",
                    "crates/a.rs",
                    12,
                    git_ref,
                    &history.pre_fix_sha,
                ),
                code_alert_at(
                    599,
                    "rust/cleartext-logging",
                    "crates/c.rs",
                    99,
                    git_ref,
                    &history.pre_fix_sha,
                ),
            ],
        ),
        None,
    );
    assert_eq!(mixed["filed_count"], 1, "{mixed}");
    assert_eq!(mixed["filed"][0]["alert_numbers"], json!([599]));
    assert_eq!(mixed["awaiting_post_fix_scan"].as_array().unwrap().len(), 1);
    assert_eq!(mixed["awaiting_post_fix_scan"][0]["alert_number"], 507);
    assert_eq!(mixed["filed"][0]["delta_covered_by"], json!([]));
}

/// Unmatched branches/refs, missing/non-fix/contradictory delivery evidence,
/// missing/refused dispositions, and diverged SHAs remain eligible.
fn unmatched_branch_or_missing_and_contradictory_evidence_remain_eligible() {
    let repo_name = "constellation-works/orbit";
    let git_ref = "refs/heads/agent-main";

    // Helper that creates one completed single-alert task and runs a replay.
    let run_case = |setup: &dyn Fn(&OrbitRuntime, &str, &CodeScanningHistory),
                    alert_fn: &dyn Fn(&CodeScanningHistory) -> Value| {
        let (root, runtime) = fixture();
        let history = init_code_scanning_git_history(root.path());
        let first = file(
            &runtime,
            orbit_code_snapshot(
                repo_name,
                vec![code_alert_at(
                    507,
                    "rust/cleartext-logging",
                    "crates/a.rs",
                    12,
                    git_ref,
                    &history.pre_fix_sha,
                )],
            ),
            None,
        );
        let owner = first["filed"][0]["task_id"].as_str().unwrap().to_string();
        setup(&runtime, &owner, &history);
        let replay = file(
            &runtime,
            orbit_code_snapshot(repo_name, vec![alert_fn(&history)]),
            None,
        );
        assert_eq!(
            replay["awaiting_post_fix_scan"],
            json!([]),
            "must not defer: {replay}"
        );
        assert_eq!(replay["filed_count"], 1, "must file afresh: {replay}");
    };

    // 1. Unmatched alert ref (refs/heads/main vs completed refs/heads/agent-main).
    run_case(
        &|runtime, id, history| {
            settle_task_external_merge(
                runtime,
                id,
                1,
                "agent-main",
                &history.landed_shas[0],
                "## Per-alert disposition\n- #507: `fixed`",
            );
        },
        &|history| {
            code_alert_at(
                507,
                "rust/cleartext-logging",
                "crates/a.rs",
                12,
                "refs/heads/main",
                &history.pre_fix_sha,
            )
        },
    );

    // 2. Unmatched delivery target branch (merged into `main` vs alert `refs/heads/agent-main`).
    run_case(
        &|runtime, id, history| {
            settle_task_external_merge(
                runtime,
                id,
                2,
                "main",
                &history.landed_shas[0],
                "## Per-alert disposition\n- #507: `fixed`",
            );
        },
        &|history| {
            code_alert_at(
                507,
                "rust/cleartext-logging",
                "crates/a.rs",
                12,
                git_ref,
                &history.pre_fix_sha,
            )
        },
    );

    // 3. Non-branch alert ref (refs/pull/42/merge).
    run_case(
        &|runtime, id, history| {
            settle_task_external_merge(
                runtime,
                id,
                3,
                "agent-main",
                &history.landed_shas[0],
                "## Per-alert disposition\n- #507: `fixed`",
            );
        },
        &|history| {
            code_alert_at(
                507,
                "rust/cleartext-logging",
                "crates/a.rs",
                12,
                "refs/pull/42/merge",
                &history.pre_fix_sha,
            )
        },
    );

    // 4. Missing delivery evidence (plain `Done` transition without landing proof).
    run_case(
        &|runtime, id, _history| {
            orbit_core::test_support::EngineHost::new(runtime)
                .apply_task_automation_update(
                    id,
                    orbit_engine::TaskAutomationUpdate {
                        status: Some(TaskStatus::Done),
                        execution_summary: Some("## Per-alert disposition\n- #507: `fixed`".into()),
                        status_note: Some("manually closed".into()),
                        ..Default::default()
                    },
                )
                .unwrap();
        },
        &|history| {
            code_alert_at(
                507,
                "rust/cleartext-logging",
                "crates/a.rs",
                12,
                git_ref,
                &history.pre_fix_sha,
            )
        },
    );

    // 5. VerifiedNoDiff delivery (no fix commit landed by this task).
    run_case(
        &|runtime, id, history| {
            orbit_core::test_support::EngineHost::new(runtime)
                .apply_task_automation_update(
                    id,
                    orbit_engine::TaskAutomationUpdate {
                        status: Some(TaskStatus::Done),
                        execution_summary: Some("## Per-alert disposition\n- #507: `fixed`".into()),
                        status_note: Some("no diff".into()),
                        ..Default::default()
                    },
                )
                .unwrap();
            install_pr_delivery_run(runtime, id, "agent-main", &history.landed_shas[0], true);
        },
        &|history| {
            code_alert_at(
                507,
                "rust/cleartext-logging",
                "crates/a.rs",
                12,
                git_ref,
                &history.pre_fix_sha,
            )
        },
    );

    // 6. Contradictory delivery evidence (run delivery vs external_merge_settled disagree).
    run_case(
        &|runtime, id, history| {
            settle_task_external_merge(
                runtime,
                id,
                6,
                "agent-main",
                &history.landed_shas[0],
                "## Per-alert disposition\n- #507: `fixed`",
            );
            install_pr_delivery_run(runtime, id, "agent-main", &history.landed_shas[1], false);
        },
        &|history| {
            code_alert_at(
                507,
                "rust/cleartext-logging",
                "crates/a.rs",
                12,
                git_ref,
                &history.pre_fix_sha,
            )
        },
    );

    // 7. Diverged analysis SHA (not an ancestor of landed_sha).
    run_case(
        &|runtime, id, history| {
            settle_task_external_merge(
                runtime,
                id,
                7,
                "agent-main",
                &history.landed_shas[0],
                "## Per-alert disposition\n- #507: `fixed`",
            );
        },
        &|history| {
            code_alert_at(
                507,
                "rust/cleartext-logging",
                "crates/a.rs",
                12,
                git_ref,
                &history.diverged_sha,
            )
        },
    );

    // 8. Missing or non-full analysis SHA.
    run_case(
        &|runtime, id, history| {
            settle_task_external_merge(
                runtime,
                id,
                8,
                "agent-main",
                &history.landed_shas[0],
                "## Per-alert disposition\n- #507: `fixed`",
            );
        },
        &|_history| {
            code_alert_at(
                507,
                "rust/cleartext-logging",
                "crates/a.rs",
                12,
                git_ref,
                "not-a-full-sha",
            )
        },
    );

    // 9. `not-reproducible-in-source` disposition does not defer.
    run_case(
        &|runtime, id, history| {
            settle_task_external_merge(
                runtime,
                id,
                9,
                "agent-main",
                &history.landed_shas[0],
                "## Per-alert disposition\n- #507: `not-reproducible-in-source` — stale line",
            );
        },
        &|history| {
            code_alert_at(
                507,
                "rust/cleartext-logging",
                "crates/a.rs",
                12,
                git_ref,
                &history.pre_fix_sha,
            )
        },
    );

    // 10. Missing per-alert disposition for #507 does not defer.
    run_case(
        &|runtime, id, history| {
            settle_task_external_merge(
                runtime,
                id,
                10,
                "agent-main",
                &history.landed_shas[0],
                "Outcome: success\nNo per-alert disposition recorded.",
            );
        },
        &|history| {
            code_alert_at(
                507,
                "rust/cleartext-logging",
                "crates/a.rs",
                12,
                git_ref,
                &history.pre_fix_sha,
            )
        },
    );

    // 11. Contradictory repository on run delivery (origin remote is another GitHub repo).
    let (root, runtime) = fixture();
    let history = init_code_scanning_git_history(root.path());
    git(
        &root.path().join("repo"),
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/acme/other.git",
        ],
    );
    let first = file(
        &runtime,
        orbit_code_snapshot(
            repo_name,
            vec![code_alert_at(
                507,
                "rust/cleartext-logging",
                "crates/a.rs",
                12,
                git_ref,
                &history.pre_fix_sha,
            )],
        ),
        None,
    );
    let owner = first["filed"][0]["task_id"].as_str().unwrap();
    orbit_core::test_support::EngineHost::new(&runtime)
        .apply_task_automation_update(
            owner,
            orbit_engine::TaskAutomationUpdate {
                status: Some(TaskStatus::Done),
                execution_summary: Some("## Per-alert disposition\n- #507: `fixed`".into()),
                status_note: Some("completed".into()),
                ..Default::default()
            },
        )
        .unwrap();
    install_pr_delivery_run(
        &runtime,
        owner,
        "agent-main",
        &history.landed_shas[0],
        false,
    );
    let replay = file(
        &runtime,
        orbit_code_snapshot(
            repo_name,
            vec![code_alert_at(
                507,
                "rust/cleartext-logging",
                "crates/a.rs",
                12,
                git_ref,
                &history.pre_fix_sha,
            )],
        ),
        None,
    );
    assert_eq!(replay["awaiting_post_fix_scan"], json!([]), "{replay}");
    assert_eq!(replay["filed_count"], 1, "{replay}");
}

// Related boundary scenarios share the isolated table fixture.
crate::scenario_table!(
    consolidation_retires_every_duplicate_alert_owner_once_scenarios,
    consolidation_retires_every_duplicate_alert_owner_once,
    filing_applies_the_fixed_floor_and_reports_its_source,
    input_overrides_the_floor_for_one_filing_and_secrets_ignore_it,
    mixed_legacy_and_typed_code_scanning_evidence_with_missing_guid_consolidates,
    rejected_code_scanning_owner_suppresses_refiling_until_the_alert_changes,
    rejected_owner_distinguishes_punctuation_only_path_and_rule_changes,
    rejected_grouped_owner_refills_alert_moved_onto_a_sibling_location,
    rejection_naming_a_covering_task_does_not_suppress_code_scanning_refiling,
    rejected_source_uses_supersedes_before_legacy_consolidation_comments,
    typed_code_scanning_evidence_consolidates_despite_edited_prose,
    sweep_tasks_seed_file_selectors_from_alert_locations,
    sweep_tasks_drop_missing_and_git_orbit_paths,
    code_scanning_rule_missing_from_installed_bundle_waits_on_one_prerequisite,
    code_scanning_without_installed_bundle_waits_on_a_prerequisite,
    pre_fix_code_scanning_replay_defers_all_18_alerts_until_post_fix_scan,
    post_fix_code_scanning_scan_and_new_alerts_remain_eligible,
    unmatched_branch_or_missing_and_contradictory_evidence_remain_eligible
);
