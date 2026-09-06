use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use orbit_types::task::{TaskPriority, TaskStatus};
use serde_json::{Value, json};
use tempfile::tempdir;

use crate::OrbitRuntime;
use crate::adapter::engine_host::v2_host::test_support::runtime_with_workspace_layout;

pub(super) fn alert(number: u64, severity: &str, range: &str, ghsa: &str) -> Value {
    json!({
        "number": number, "state": "open", "ecosystem": "cargo", "package": "time",
        "manifest_path": "Cargo.lock", "scope": "runtime", "severity": severity,
        "ghsa_id": ghsa, "cve_id": format!("CVE-2026-{number}"),
        "summary": format!("vulnerability {number}"), "vulnerable_version_range": range,
        "first_patched_version": "1.2.3", "html_url": format!("https://github.test/alerts/{number}")
    })
}

pub(super) fn snapshot(alerts: Vec<Value>, pull_requests: Vec<Value>) -> Value {
    json!({
        "schema_version": 1, "collected": true,
        "outcome_hint": if alerts.is_empty() { "no_open_alerts" } else { "open_alerts" },
        "capability": {"available": true, "authenticated": true, "detail": "authenticated"},
        "repository": {"full_name": "acme/orbit"},
        "open_alerts": alerts,
        "open_dependabot_pull_requests": pull_requests,
        "query_errors": [],
        "truncation": {"alerts_limit": 100, "alerts_at_cap": false},
        "collected_at": "2026-08-31T00:00:00Z"
    })
}

pub(super) fn expanded_snapshot(
    dependabot: Vec<Value>,
    code_scanning: Vec<Value>,
    secret_scanning: Vec<Value>,
) -> Value {
    json!({
        "schema_version": 2,
        "collected": true,
        "collection_status": "fully_collected",
        "outcome_hint": if dependabot.is_empty() { "no_open_alerts" } else { "open_alerts" },
        "capability": {"available": true, "authenticated": true, "detail": "authenticated"},
        "repository": {"full_name": "acme/orbit"},
        "open_alerts": dependabot,
        "open_dependabot_pull_requests": [],
        "query_errors": [],
        "truncation": {"alerts_limit": 100, "alerts_at_cap": false},
        "code_scanning": {
            "collected": true,
            "collection_status": "fully_collected",
            "outcome_hint": if code_scanning.is_empty() { "no_open_alerts" } else { "open_alerts" },
            "capability": {"available": true, "authenticated": true},
            "open_alerts": code_scanning,
            "query_errors": [],
            "truncation": {"alerts_limit": 100, "alerts_at_cap": false}
        },
        "secret_scanning": {
            "collected": true,
            "collection_status": "fully_collected",
            "outcome_hint": if secret_scanning.is_empty() { "no_open_alerts" } else { "open_alerts" },
            "capability": {"available": true, "authenticated": true},
            "open_alerts": secret_scanning,
            "query_errors": [],
            "truncation": {"alerts_limit": 100, "alerts_at_cap": false, "locations_limit_per_alert": 20}
        },
        "collected_at": "2026-08-31T00:00:00Z"
    })
}

pub(super) fn code_alert(number: u64, severity: &str) -> Value {
    json!({
        "number": number, "state": "open", "rule_id": "rust/sql-injection",
        "rule_name": "SQL injection", "rule_description": "Untrusted input reaches SQL",
        "security_severity": severity, "tool_name": "CodeQL", "tool_guid": "codeql-guid",
        "tool_version": "2.20", "message": "User-controlled value reaches query",
        "ref": "refs/heads/main", "commit_sha": "abc123", "path": "src/db.rs",
        "start_line": 17, "end_line": 19, "start_column": 4, "end_column": 20,
        "created_at": "2026-08-30T00:00:00Z", "updated_at": "2026-08-31T00:00:00Z",
        "html_url": format!("https://github.test/code/{number}")
    })
}

fn secret_alert(number: u64, validity: &str) -> Value {
    json!({
        "number": number, "state": "open", "secret_type": "example_token",
        "secret_type_display_name": "Example token", "validity": validity,
        "publicly_leaked": true, "multi_repo": false,
        "created_at": "2026-08-30T00:00:00Z", "updated_at": "2026-08-31T00:00:00Z",
        "html_url": format!("https://github.test/secret/{number}"),
        "locations": [{"type": "commit", "path": "config/dev.env", "start_line": 3,
            "end_line": 3, "commit_sha": "def456", "commit_url": "https://github.test/commit/def456"}],
        "locations_at_cap": false
    })
}

pub(super) fn file(runtime: &OrbitRuntime, snapshot: Value, extra: Value) -> Value {
    let mut input = json!({"dependabot_snapshot": snapshot});
    if let (Some(target), Some(source)) = (input.as_object_mut(), extra.as_object()) {
        target.extend(source.clone());
    }
    runtime
        .run_deterministic(
            "file_dependabot_alert_tasks",
            &json!({}),
            &input,
            ToolContext::default(),
        )
        .expect("file Dependabot tasks")
}

fn runtime_without_system_crew() -> (tempfile::TempDir, OrbitRuntime) {
    let root = tempdir().expect("create tempdir");
    let global = root.path().join("home/.orbit");
    let workspace = root.path().join("repo/.orbit");
    std::fs::create_dir_all(&global).expect("global orbit dir");
    std::fs::create_dir_all(&workspace).expect("workspace orbit dir");
    std::fs::write(
        workspace.join("config.toml"),
        r#"[workflow]
default_crew = "sol"
system_crew = "not-a-real-crew"

[crews.sol]
provider = "codex"
model = "gpt-5.6-sol"
"#,
    )
    .expect("write crew config");
    let runtime = OrbitRuntime::from_roots(&global, &workspace).expect("build runtime");
    (root, runtime)
}

#[test]
fn several_alerts_for_one_dependency_file_exactly_one_evidence_complete_task() {
    let (_root, runtime, _repo) = runtime_with_workspace_layout();
    let output = file(
        &runtime,
        snapshot(
            vec![
                alert(1, "high", "< 1.0.0", "GHSA-one"),
                alert(2, "critical", "< 1.2.3", "GHSA-two"),
            ],
            Vec::new(),
        ),
        json!({}),
    );
    assert_eq!(output["filed_count"], json!(1));
    assert_eq!(output["clusters"], json!(1));
    let task_id = output["filed"][0]["task_id"].as_str().expect("task id");
    let task = runtime.get_task(task_id).expect("filed task");
    assert_eq!(task.status, TaskStatus::Backlog);
    assert_eq!(task.priority, TaskPriority::Critical);
    assert!(task.required_tools.is_empty());
    assert!(task.title.starts_with("[dependabot-sweep] "));
    for expected in [
        "time",
        "Cargo.lock",
        "1.2.3",
        "GHSA-one",
        "CVE-2026-1",
        "GHSA-two",
        "https://github.test/alerts/2",
    ] {
        assert!(task.description.contains(expected), "missing {expected}");
    }
}

#[test]
fn dedupe_key_survives_changed_alert_number_severity_and_range() {
    let (_root, runtime, _repo) = runtime_with_workspace_layout();
    let first = file(
        &runtime,
        snapshot(vec![alert(1, "high", "< 1.0.0", "GHSA-old")], Vec::new()),
        json!({}),
    );
    assert_eq!(first["filed_count"], json!(1));
    let second = file(
        &runtime,
        snapshot(
            vec![alert(99, "critical", "< 9.9.9", "GHSA-new")],
            Vec::new(),
        ),
        json!({}),
    );
    assert_eq!(second["filed_count"], json!(0));
    assert_eq!(
        second["skipped_existing"]
            .as_array()
            .expect("skipped")
            .len(),
        1
    );
    assert_eq!(
        first["filed"][0]["key"],
        second["skipped_existing"][0]["key"]
    );
    assert_eq!(second["skipped_existing"][0]["match_kind"], "exact_key");
}

#[test]
fn severity_floor_is_reported_and_dependabot_pr_skip_is_switchable() {
    let (_root, runtime, _repo) = runtime_with_workspace_layout();
    let low = file(
        &runtime,
        snapshot(
            vec![alert(1, "moderate", "< 1.0.0", "GHSA-low")],
            Vec::new(),
        ),
        json!({}),
    );
    assert_eq!(low["filed_count"], json!(0));
    assert_eq!(
        low["excluded_below_min_severity"]
            .as_array()
            .expect("excluded")
            .len(),
        1
    );

    let pr = json!({"number": 4, "title": "Bump time from 1.0.0 to 1.2.3", "body": "Updates time", "url": "https://github.test/pr/4", "author": "app/dependabot"});
    let skipped = file(
        &runtime,
        snapshot(
            vec![alert(2, "high", "< 1.2.3", "GHSA-high")],
            vec![pr.clone()],
        ),
        json!({}),
    );
    assert_eq!(skipped["filed_count"], json!(0));
    assert_eq!(
        skipped["skipped_dependabot_pr"]
            .as_array()
            .expect("PR skips")
            .len(),
        1
    );

    let filed = file(
        &runtime,
        snapshot(vec![alert(2, "high", "< 1.2.3", "GHSA-high")], vec![pr]),
        json!({"skip_when_dependabot_pr_open": false}),
    );
    assert_eq!(filed["filed_count"], json!(1));
    assert_eq!(filed["skipped_dependabot_pr"], json!([]));
}

#[test]
fn open_pr_for_an_unrelated_package_does_not_suppress_filing() {
    let (_root, runtime, _repo) = runtime_with_workspace_layout();
    // `time` is a bare substring of both the PR title's release-notes prose
    // and this changelog-flavored body, but the PR bumps `tokio`, not `time`.
    let pr = json!({
        "number": 5,
        "title": "Bump tokio from 1.28.0 to 1.28.1",
        "body": "Bumps tokio. Release notes mention improvements to runtime scheduling and the time crate.",
        "url": "https://github.test/pr/5",
        "author": "app/dependabot",
        "head_branch": "dependabot/cargo/tokio-1.28.1",
    });
    let output = file(
        &runtime,
        snapshot(vec![alert(1, "high", "< 1.2.3", "GHSA-time")], vec![pr]),
        json!({}),
    );
    assert_eq!(output["filed_count"], json!(1));
    assert_eq!(output["skipped_dependabot_pr"], json!([]));
}

#[test]
fn open_pr_that_bumps_the_alerting_package_still_suppresses_filing() {
    let (_root, runtime, _repo) = runtime_with_workspace_layout();
    let by_title = json!({
        "number": 6,
        "title": "Bump time from 1.0.0 to 1.2.3",
        "body": "Bumps time.",
        "url": "https://github.test/pr/6",
        "author": "app/dependabot",
        "head_branch": "",
    });
    let title_match = file(
        &runtime,
        snapshot(
            vec![alert(1, "high", "< 1.2.3", "GHSA-title")],
            vec![by_title],
        ),
        json!({}),
    );
    assert_eq!(title_match["filed_count"], json!(0));
    assert_eq!(
        title_match["skipped_dependabot_pr"]
            .as_array()
            .expect("PR skips")
            .len(),
        1
    );

    let by_branch = json!({
        "number": 7,
        "title": "Bump the cargo group with 2 updates",
        "body": "Bumps time and another package.",
        "url": "https://github.test/pr/7",
        "author": "app/dependabot",
        "head_branch": "dependabot/cargo/time-1.2.3",
    });
    let branch_match = file(
        &runtime,
        snapshot(
            vec![alert(2, "high", "< 1.2.3", "GHSA-branch")],
            vec![by_branch],
        ),
        json!({}),
    );
    assert_eq!(branch_match["filed_count"], json!(0));
    assert_eq!(
        branch_match["skipped_dependabot_pr"]
            .as_array()
            .expect("PR skips")
            .len(),
        1
    );
}

#[test]
fn filing_succeeds_without_a_system_crew() {
    let (_root, runtime) = runtime_without_system_crew();
    let output = file(
        &runtime,
        snapshot(
            vec![alert(7, "high", "< 1.2.3", "GHSA-no-crew")],
            Vec::new(),
        ),
        json!({}),
    );
    assert_eq!(output["filed_count"], json!(1));
    let task_id = output["filed"][0]["task_id"].as_str().expect("task id");
    assert_eq!(runtime.get_task(task_id).expect("task").crew, None);
}

#[test]
fn code_and_secret_alerts_file_evidence_complete_deduplicated_tasks() {
    let (_root, runtime, _repo) = runtime_with_workspace_layout();
    let snapshot = expanded_snapshot(
        Vec::new(),
        vec![code_alert(8, "high")],
        vec![secret_alert(9, "active")],
    );
    let first = file(&runtime, snapshot.clone(), json!({}));
    assert_eq!(first["filed_count"], json!(2));
    assert_eq!(first["outcome"], "open_alerts");
    assert_eq!(first["collection_outcome"], "fully_collected");

    let code_id = first["filed"]
        .as_array()
        .expect("filed")
        .iter()
        .find(|entry| entry["family"] == "code_scanning")
        .and_then(|entry| entry["task_id"].as_str())
        .expect("code task id");
    let code = runtime.get_task(code_id).expect("code task");
    assert!(code.title.starts_with("[code-scanning-sweep] "));
    assert_eq!(code.status, TaskStatus::Backlog);
    assert_eq!(code.priority, TaskPriority::High);
    assert!(code.required_tools.is_empty());
    for evidence in [
        "rust/sql-injection",
        "CodeQL",
        "User-controlled value reaches query",
        "refs/heads/main",
        "abc123",
        "src/db.rs",
        "lines 17-19",
        "https://github.test/code/8",
    ] {
        assert!(code.description.contains(evidence), "missing {evidence}");
    }
    assert!(
        code.acceptance_criteria
            .iter()
            .any(|criterion| criterion.contains("do not suppress"))
    );

    let secret_id = first["filed"]
        .as_array()
        .expect("filed")
        .iter()
        .find(|entry| entry["family"] == "secret_scanning")
        .and_then(|entry| entry["task_id"].as_str())
        .expect("secret task id");
    let secret = runtime.get_task(secret_id).expect("secret task");
    assert!(secret.title.starts_with("[secret-scanning-sweep] "));
    assert_eq!(secret.priority, TaskPriority::Critical);
    assert!(secret.required_tools.is_empty());
    assert!(secret.description.contains("config/dev.env"));
    assert!(
        secret
            .acceptance_criteria
            .iter()
            .any(|criterion| criterion.contains("Rotate or revoke"))
    );

    let second = file(&runtime, snapshot, json!({}));
    assert_eq!(second["filed_count"], json!(0));
    assert_eq!(
        second["skipped_existing"]
            .as_array()
            .expect("existing")
            .len(),
        2
    );
    assert!(
        second["skipped_existing"]
            .as_array()
            .expect("existing")
            .iter()
            .all(|entry| entry.get("family").is_some())
    );
}

#[test]
fn code_scanning_severity_floor_excludes_lower_severity_alerts() {
    let (_root, runtime, _repo) = runtime_with_workspace_layout();
    let output = file(
        &runtime,
        expanded_snapshot(
            Vec::new(),
            vec![code_alert(10, "moderate"), code_alert(11, "high")],
            Vec::new(),
        ),
        json!({"min_severity": "high"}),
    );

    assert_eq!(output["filed_count"], json!(1));
    assert_eq!(output["filed"][0]["alert_number"], json!(11));
    assert_eq!(
        output["excluded_below_min_severity"],
        json!([{
            "family": "code_scanning",
            "alert_number": 10,
            "rule_id": "rust/sql-injection",
            "path": "src/db.rs",
            "security_severity": "moderate",
            "reason": "below_min_severity",
        }])
    );
}

#[test]
fn code_scanning_missing_or_unrecognized_severity_is_explicitly_excluded() {
    let (_root, runtime, _repo) = runtime_with_workspace_layout();
    let mut missing = code_alert(12, "high");
    missing
        .as_object_mut()
        .expect("code alert object")
        .remove("security_severity");
    let output = file(
        &runtime,
        expanded_snapshot(
            Vec::new(),
            vec![missing, code_alert(13, "informational")],
            Vec::new(),
        ),
        json!({"min_severity": "low"}),
    );

    assert_eq!(output["filed_count"], json!(0));
    let excluded = output["excluded_below_min_severity"]
        .as_array()
        .expect("excluded alerts");
    assert_eq!(excluded.len(), 2);
    assert_eq!(excluded[0]["alert_number"], json!(12));
    assert_eq!(excluded[0]["security_severity"], "");
    assert_eq!(
        excluded[0]["reason"],
        "missing_or_unrecognized_security_severity"
    );
    assert_eq!(excluded[1]["alert_number"], json!(13));
    assert_eq!(excluded[1]["security_severity"], "informational");
    assert_eq!(
        excluded[1]["reason"],
        "missing_or_unrecognized_security_severity"
    );
}

#[test]
fn max_tasks_is_one_deterministic_bound_across_all_families() {
    let (_root, runtime, _repo) = runtime_with_workspace_layout();
    let output = file(
        &runtime,
        expanded_snapshot(
            vec![alert(1, "high", "< 1.2.3", "GHSA-first")],
            vec![code_alert(2, "critical")],
            vec![secret_alert(3, "active")],
        ),
        json!({"max_tasks": 1}),
    );
    assert_eq!(output["filed_count"], json!(1));
    assert_eq!(output["filed"][0]["family"], "dependabot");
    let skipped = output["skipped_over_cap"].as_array().expect("over cap");
    assert_eq!(skipped.len(), 2);
    assert_eq!(skipped[0]["family"], "code_scanning");
    assert_eq!(skipped[1]["family"], "secret_scanning");
}

#[test]
fn sentinel_credential_never_reaches_snapshot_output_or_persisted_task_fields() {
    const SENTINEL: &str = "orbit-sentinel-credential-2ce944";
    let projected = orbit_tools::github_cli::project_secret_scanning_alert(&json!({
        "number": 77, "state": "open", "secret_type": "example_token",
        "secret_type_display_name": "Example token", "secret": SENTINEL,
        "validity": "active", "publicly_leaked": false, "multi_repo": false,
        "created_at": "2026-08-30T00:00:00Z", "updated_at": "2026-08-31T00:00:00Z",
        "html_url": "https://github.test/secret/77"
    }));
    let mut projected = projected;
    projected["locations"] = json!([{
        "type": "commit", "path": "config/dev.env", "start_line": 3,
        "end_line": 3, "commit_sha": "def456", "commit_url": "https://github.test/commit/def456"
    }]);
    projected["locations_at_cap"] = json!(false);
    let snapshot = expanded_snapshot(Vec::new(), Vec::new(), vec![projected]);
    assert!(
        !serde_json::to_string(&snapshot)
            .expect("snapshot")
            .contains(SENTINEL)
    );

    let (_root, runtime, _repo) = runtime_with_workspace_layout();
    let output = file(&runtime, snapshot, json!({}));
    assert!(
        !serde_json::to_string(&output)
            .expect("output")
            .contains(SENTINEL)
    );
    let task_id = output["filed"][0]["task_id"].as_str().expect("task id");
    let task = runtime.get_task(task_id).expect("task");
    let persisted = json!({
        "title": task.title,
        "description": task.description,
        "acceptance_criteria": task.acceptance_criteria,
        "tags": task.tags,
        "required_tools": task.required_tools,
        "error": null,
        "artifacts": [],
        "captured_logs": [],
    });
    assert!(
        !serde_json::to_string(&persisted)
            .expect("persisted")
            .contains(SENTINEL)
    );
}

#[test]
fn unavailable_family_does_not_hide_findings_from_collected_family() {
    let (_root, runtime, _repo) = runtime_with_workspace_layout();
    let mut snapshot = expanded_snapshot(Vec::new(), vec![code_alert(15, "high")], Vec::new());
    snapshot["collection_status"] = json!("partially_collected");
    snapshot["secret_scanning"] = json!({
        "collected": false,
        "collection_status": "capability_unavailable",
        "outcome_hint": "capability_unavailable",
        "capability": {"available": false, "authenticated": true, "detail": "permission unavailable"}
    });
    let output = file(&runtime, snapshot, json!({}));
    assert_eq!(output["filed_count"], json!(1));
    assert_eq!(output["filed"][0]["family"], "code_scanning");
    assert_eq!(output["collection_outcome"], "partially_collected");
    assert_eq!(
        output["family_outcomes"]["secret_scanning"]["outcome"],
        "capability_unavailable"
    );
}
