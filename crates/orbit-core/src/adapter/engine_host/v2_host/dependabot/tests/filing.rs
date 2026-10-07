use orbit_engine::RuntimeHost;
use orbit_tools::ToolContext;
use orbit_types::task::TaskComplexity;
use serde_json::{Value, json};

use crate::OrbitRuntime;
use crate::adapter::engine_host::v2_host::test_support::{
    runtime_with_workspace_config, runtime_with_workspace_layout,
};

pub(in crate::adapter::engine_host::v2_host) fn expanded_snapshot(
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

pub(in crate::adapter::engine_host::v2_host) fn file(
    runtime: &OrbitRuntime,
    snapshot: Value,
    extra: Value,
) -> Value {
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
fn dependabot_bump_uses_low_pool_without_changing_identity_tag() {
    if crate::application::run_isolated_test(std::any::type_name_of_val(
        &dependabot_bump_uses_low_pool_without_changing_identity_tag,
    )) {
        return;
    }

    let (_root, runtime, _repo) = runtime_with_workspace_config(Some(
        r#"[workflow]
default_crew = "system"
low_complexity_crews = ["fixture"]

[crews.fixture]
provider = "codex"
model = "fixture-model"
backend = "cli"

[crews.system]
provider = "codex"
model = "system-model"
backend = "cli"
"#,
    ));
    let snapshot = expanded_snapshot(
        vec![json!({
            "number": 73,
            "state": "open",
            "severity": "high",
            "ecosystem": "npm",
            "package": "source-map-js",
            "manifest_path": "website/package-lock.json",
            "vulnerable_range": "<1.2.2",
            "first_patched_version": "1.2.2"
        })],
        Vec::new(),
        Vec::new(),
    );

    let output = file(&runtime, snapshot, json!({}));
    let entry = &output["filed"][0];
    let key = entry["key"].as_str().expect("filed identity key");
    let task = runtime
        .get_task(entry["task_id"].as_str().expect("filed task id"))
        .expect("persisted task");

    assert_eq!(task.complexity, Some(TaskComplexity::Low));
    assert_eq!(task.crew.as_deref(), Some("fixture"));
    assert!(task.tags.contains(&format!("dependabot:{key}")));
}

#[test]
fn dependabot_alert_ecosystems_match_open_pr_branch_names() {
    if crate::application::run_isolated_test(std::any::type_name_of_val(
        &dependabot_alert_ecosystems_match_open_pr_branch_names,
    )) {
        return;
    }

    let (_root, runtime, _repo) = runtime_with_workspace_layout();
    for (ecosystem, branch_ecosystem, package, manifest_path) in [
        (
            "npm",
            "npm_and_yarn",
            "source-map-js",
            "website/package-lock.json",
        ),
        ("rust", "cargo", "serde", "Cargo.lock"),
        (
            "actions",
            "github_actions",
            "actions-checkout",
            ".github/workflows/ci.yml",
        ),
        ("go", "go_modules", "go-example", "go.mod"),
        ("rubygems", "bundler", "rack", "Gemfile.lock"),
    ] {
        let mut snapshot = expanded_snapshot(
            vec![json!({
                "number": 100,
                "state": "open",
                "severity": "high",
                "ecosystem": ecosystem,
                "package": package,
                "manifest_path": manifest_path,
                "vulnerable_range": "<1.2.2",
                "first_patched_version": "1.2.2"
            })],
            Vec::new(),
            Vec::new(),
        );
        snapshot["open_dependabot_pull_requests"] = json!([{
            "title": format!("chore(deps): update {package}"),
            "head_branch": format!(
                "dependabot/{branch_ecosystem}/{manifest_path}/{package}-1.2.2"
            )
        }]);

        let output = file(&runtime, snapshot, json!({}));
        assert_eq!(output["filed_count"], 0, "alert ecosystem {ecosystem}");
        assert_eq!(
            output["skipped_dependabot_pr"].as_array().map(Vec::len),
            Some(1),
            "alert ecosystem {ecosystem} should match its Dependabot branch"
        );
    }
}
